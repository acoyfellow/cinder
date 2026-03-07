use serde::{Deserialize, Serialize};
use wasm_bindgen::JsValue;
use worker::*;
use worker::kv::KvStore;

use crate::AppState;

const REPO_KEY_PREFIX: &str = "connected_repo:";

#[derive(Debug, Deserialize)]
pub struct ConnectRepoRequest {
    repo: String,
    branch: String,
    workflow: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ConnectedRepoState {
    repo: String,
    branch: String,
    workflow: String,
    labels: Vec<String>,
    webhook_status: String,
    connection_status: String,
    connected_at: i64,
    last_dispatch_status: Option<String>,
    last_dispatch_run_id: Option<u64>,
    last_dispatch_requested_at: Option<String>,
}

fn require_internal_token(req: &Request, ctx: &RouteContext<AppState>) -> Result<()> {
    let token = req
        .headers()
        .get("Authorization")?
        .ok_or_else(|| worker::Error::RustError("missing auth".into()))?;

    if token != format!("Bearer {}", ctx.data.internal_token) {
        return Err(worker::Error::RustError("unauthorized".into()));
    }

    Ok(())
}

fn parse_repo_ref(repo_ref: &str) -> Result<(String, String)> {
    let parts = repo_ref.split('/').collect::<Vec<_>>();
    if parts.len() != 2 || parts.iter().any(|part| part.trim().is_empty()) {
        return Err(worker::Error::RustError(format!(
            "repo must be owner/name but received {repo_ref}"
        )));
    }

    Ok((parts[0].to_string(), parts[1].to_string()))
}

fn repo_state_key(repo: &str) -> String {
    format!("{REPO_KEY_PREFIX}{repo}")
}

async fn load_connected_repo_state(kv: &KvStore, repo_ref: &str) -> Result<Option<ConnectedRepoState>> {
    kv.get(&repo_state_key(repo_ref))
        .json::<ConnectedRepoState>()
        .await
        .map_err(worker::Error::from)
}

async fn save_connected_repo_state(kv: &KvStore, state: &ConnectedRepoState) -> Result<()> {
    kv.put(&repo_state_key(&state.repo), serde_json::to_string(state)?)?
        .execute()
        .await
        .map_err(|error| worker::Error::RustError(error.to_string()))
}

fn encode_workflow_path(path: &str) -> String {
    path.split('/')
        .map(js_sys::encode_uri_component)
        .map(String::from)
        .collect::<Vec<_>>()
        .join("/")
}

fn encode_workflow_id(workflow: &str) -> String {
    js_sys::encode_uri_component(workflow).into()
}

async fn github_request(
    ctx: &RouteContext<AppState>,
    path: &str,
    method: Method,
    body: Option<String>,
    ok_statuses: &[u16],
) -> Result<Response> {
    let mut init = RequestInit::new();
    init.with_method(method.clone());

    if let Some(body) = body.as_ref() {
        init.with_body(Some(JsValue::from_str(body)));
    }

    let mut request = Request::new_with_init(&format!("https://api.github.com{path}"), &init)?;
    let headers = request.headers_mut()?;
    headers.set("Accept", "application/vnd.github+json")?;
    headers.set("Authorization", &format!("Bearer {}", ctx.data.github_pat))?;
    headers.set("User-Agent", "cinder-orchestrator")?;
    headers.set("X-GitHub-Api-Version", "2022-11-28")?;

    let mut response = Fetch::Request(request).send().await?;
    if ok_statuses.contains(&response.status_code()) {
        return Ok(response);
    }

    let status = response.status_code();
    let text = response.text().await.unwrap_or_default();
    Err(worker::Error::RustError(format!(
        "GitHub API {method:?} {path} failed with {status}: {text}"
    )))
}

async fn ensure_existing_target(
    ctx: &RouteContext<AppState>,
    owner: &str,
    repo: &str,
    branch: &str,
    workflow: &str,
) -> Result<()> {
    let repo_response = github_request(ctx, &format!("/repos/{owner}/{repo}"), Method::Get, None, &[200, 404]).await?;
    if repo_response.status_code() == 404 {
        return Err(worker::Error::RustError(format!(
            "repo {owner}/{repo} does not exist or is not accessible"
        )));
    }

    let branch_response = github_request(
        ctx,
        &format!(
            "/repos/{owner}/{repo}/git/ref/heads/{}",
            js_sys::encode_uri_component(branch)
        ),
        Method::Get,
        None,
        &[200, 404],
    )
    .await?;
    if branch_response.status_code() == 404 {
        return Err(worker::Error::RustError(format!(
            "branch {branch} does not exist in {owner}/{repo}"
        )));
    }

    let workflow_response = github_request(
        ctx,
        &format!(
            "/repos/{owner}/{repo}/contents/{}?ref={}",
            encode_workflow_path(workflow),
            js_sys::encode_uri_component(branch)
        ),
        Method::Get,
        None,
        &[200, 404],
    )
    .await?;
    if workflow_response.status_code() == 404 {
        return Err(worker::Error::RustError(format!(
            "workflow {workflow} does not exist in {owner}/{repo}@{branch}"
        )));
    }

    Ok(())
}

async fn upsert_webhook(
    ctx: &RouteContext<AppState>,
    owner: &str,
    repo: &str,
    webhook_url: &str,
) -> Result<()> {
    let mut hooks_response =
        github_request(ctx, &format!("/repos/{owner}/{repo}/hooks"), Method::Get, None, &[200])
            .await?;
    let hooks: Vec<serde_json::Value> = hooks_response.json().await?;
    let existing = hooks.iter().find(|hook| {
        hook["name"].as_str() == Some("web")
            && hook["config"]["url"].as_str() == Some(webhook_url)
    });

    let body = serde_json::json!({
        "active": true,
        "events": ["workflow_job"],
        "config": {
            "url": webhook_url,
            "content_type": "json",
            "insecure_ssl": "0",
            "secret": ctx.data.webhook_secret,
        }
    })
    .to_string();

    if let Some(existing) = existing {
        let id = existing["id"]
            .as_u64()
            .ok_or_else(|| worker::Error::RustError("hook id missing".into()))?;
        github_request(
            ctx,
            &format!("/repos/{owner}/{repo}/hooks/{id}"),
            Method::Patch,
            Some(body),
            &[200],
        )
        .await?;
        return Ok(());
    }

    github_request(
        ctx,
        &format!("/repos/{owner}/{repo}/hooks"),
        Method::Post,
        Some(body),
        &[201],
    )
    .await?;

    Ok(())
}

pub async fn connect(mut req: Request, ctx: RouteContext<AppState>) -> Result<Response> {
    if let Err(worker::Error::RustError(message)) = require_internal_token(&req, &ctx) {
        return Response::error(&message, 401);
    }

    let body: ConnectRepoRequest = req.json().await?;
    let (owner, repo_name) = parse_repo_ref(&body.repo)?;
    ensure_existing_target(&ctx, &owner, &repo_name, &body.branch, &body.workflow).await?;

    let host = req
        .headers()
        .get("Host")?
        .ok_or_else(|| worker::Error::RustError("missing host".into()))?;
    let webhook_url = format!("https://{host}/webhook/github");
    upsert_webhook(&ctx, &owner, &repo_name, &webhook_url).await?;

    let state = ConnectedRepoState {
        repo: body.repo,
        branch: body.branch,
        workflow: body.workflow,
        labels: vec!["self-hosted".into(), "cinder".into()],
        webhook_status: "connected".into(),
        connection_status: "connected".into(),
        connected_at: Date::now().as_millis() as i64,
        last_dispatch_status: None,
        last_dispatch_run_id: None,
        last_dispatch_requested_at: None,
    };

    let kv = ctx.kv("RUNNER_STATE")?;
    save_connected_repo_state(&kv, &state).await?;

    Response::from_json(&state)
}

pub async fn list(req: Request, ctx: RouteContext<AppState>) -> Result<Response> {
    if let Err(worker::Error::RustError(message)) = require_internal_token(&req, &ctx) {
        return Response::error(&message, 401);
    }

    let kv = ctx.kv("RUNNER_STATE")?;
    let listed = kv
        .list()
        .prefix(REPO_KEY_PREFIX.to_string())
        .execute()
        .await
        .map_err(|error| worker::Error::RustError(error.to_string()))?;

    let mut repos = Vec::new();
    for key in listed.keys {
        if let Some(state) = kv.get(&key.name).json::<ConnectedRepoState>().await? {
            repos.push(state);
        }
    }

    repos.sort_by(|left, right| left.repo.cmp(&right.repo));

    Response::from_json(&repos)
}

pub async fn state(req: Request, ctx: RouteContext<AppState>) -> Result<Response> {
    if let Err(worker::Error::RustError(message)) = require_internal_token(&req, &ctx) {
        return Response::error(&message, 401);
    }

    let owner = ctx.param("owner").map_or("", |value| value.as_str());
    let repo = ctx.param("repo").map_or("", |value| value.as_str());
    let repo_ref = format!("{owner}/{repo}");

    let kv = ctx.kv("RUNNER_STATE")?;
    let state = load_connected_repo_state(&kv, &repo_ref).await?;

    match state {
        Some(state) => Response::from_json(&state),
        None => Response::error("not found", 404),
    }
}

pub async fn dispatch(req: Request, ctx: RouteContext<AppState>) -> Result<Response> {
    if let Err(worker::Error::RustError(message)) = require_internal_token(&req, &ctx) {
        return Response::error(&message, 401);
    }

    let owner = ctx.param("owner").map_or("", |value| value.as_str());
    let repo = ctx.param("repo").map_or("", |value| value.as_str());
    let repo_ref = format!("{owner}/{repo}");
    let kv = ctx.kv("RUNNER_STATE")?;
    let mut state = match load_connected_repo_state(&kv, &repo_ref).await? {
        Some(state) => state,
        None => return Response::error("not found", 404),
    };

    let workflow_id = encode_workflow_id(&state.workflow);
    let branch = js_sys::encode_uri_component(&state.branch);
    let mut runs_response = github_request(
        &ctx,
        &format!(
            "/repos/{owner}/{repo}/actions/workflows/{workflow_id}/runs?event=workflow_dispatch&branch={branch}&per_page=20"
        ),
        Method::Get,
        None,
        &[200],
    )
    .await?;
    let runs_payload: serde_json::Value = runs_response.json().await?;
    let before_max = runs_payload["workflow_runs"]
        .as_array()
        .map(|runs| {
            runs.iter().fold(0_u64, |highest, run| {
                let run_id = run["id"].as_u64().unwrap_or(0);
                highest.max(run_id)
            })
        })
        .unwrap_or(0);

    github_request(
        &ctx,
        &format!("/repos/{owner}/{repo}/actions/workflows/{workflow_id}/dispatches"),
        Method::Post,
        Some(serde_json::json!({ "ref": state.branch }).to_string()),
        &[204],
    )
    .await?;

    let requested_at = js_sys::Date::new_0().to_iso_string().as_string().unwrap_or_default();
    let deadline = Date::now().as_millis() + 300_000;
    let mut run_id = None;
    while Date::now().as_millis() < deadline {
        let mut response = github_request(
            &ctx,
            &format!(
                "/repos/{owner}/{repo}/actions/workflows/{workflow_id}/runs?event=workflow_dispatch&branch={branch}&per_page=20"
            ),
            Method::Get,
            None,
            &[200],
        )
        .await?;
        let payload: serde_json::Value = response.json().await?;
        run_id = payload["workflow_runs"].as_array().and_then(|runs| {
            runs.iter().find_map(|run| {
                let candidate = run["id"].as_u64()?;
                (candidate > before_max).then_some(candidate)
            })
        });
        if run_id.is_some() {
            break;
        }
        worker::Delay::from(std::time::Duration::from_secs(2)).await;
    }

    let run_id = match run_id {
        Some(run_id) => run_id,
        None => {
            return Response::error("no fresh workflow run was created after dispatch", 504);
        }
    };

    state.last_dispatch_status = Some("requested".into());
    state.last_dispatch_run_id = Some(run_id);
    state.last_dispatch_requested_at = Some(requested_at);
    save_connected_repo_state(&kv, &state).await?;

    Response::from_json(&state)
}
