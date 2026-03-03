use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use wasm_bindgen::JsValue;
use worker::*;

use crate::AppState;
use crate::job_queue_do::QueuedJob;

#[derive(Debug, Serialize)]
struct ExecutionReadyJob {
    job_id: Option<u64>,
    run_id: Option<u64>,
    repo_full_name: Option<String>,
    repo_clone_url: Option<String>,
    labels: Vec<String>,
    runner_registration_url: Option<String>,
    runner_registration_token: Option<String>,
    runner_registration_expires_at: Option<String>,
    cache_key: Option<String>,
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

fn no_work() -> ExecutionReadyJob {
    ExecutionReadyJob {
        job_id: None,
        run_id: None,
        repo_full_name: None,
        repo_clone_url: None,
        labels: Vec::new(),
        runner_registration_url: None,
        runner_registration_token: None,
        runner_registration_expires_at: None,
        cache_key: None,
    }
}

async fn read_queue(ctx: &RouteContext<AppState>, path: &str) -> Result<QueuedJob> {
    let queue = ctx.env.durable_object("JOB_QUEUE")?;
    let queue_id = queue.id_from_name("default")?;
    let stub = queue_id.get_stub()?;
    let queue_response = stub.fetch_with_str(path).await?;

    if queue_response.status_code() >= 400 {
        return Err(worker::Error::RustError("failed to read job queue".into()));
    }

    let mut queue_response = queue_response;
    queue_response.json().await
}

async fn github_request(
    ctx: &RouteContext<AppState>,
    path: &str,
    method: Method,
    body: Option<String>,
    ok_statuses: &[u16],
) -> Result<Response> {
    let mut init = RequestInit::new();
    init.with_method(method);

    if let Some(body) = body.as_ref() {
        init.with_body(Some(JsValue::from_str(body)));
    }

    let mut request = Request::new_with_init(&format!("https://api.github.com{path}"), &init)?;
    let headers = request.headers_mut()?;
    headers.set("Accept", "application/vnd.github+json")?;
    headers.set("Authorization", &format!("Bearer {}", ctx.data.github_pat))?;
    headers.set("User-Agent", "cinder-orchestrator")?;
    headers.set("X-GitHub-Api-Version", "2022-11-28")?;

    if body.is_some() {
        headers.set("Content-Type", "application/json")?;
    }

    let mut response = Fetch::Request(request).send().await?;

    if ok_statuses.contains(&response.status_code()) {
        return Ok(response);
    }

    let status_code = response.status_code();
    let message = response.text().await.unwrap_or_else(|_| String::new());

    Err(worker::Error::RustError(format!(
        "GitHub API {path} failed with {status_code}: {message}"
    )))
}

async fn prepare_job(job: QueuedJob, ctx: &RouteContext<AppState>) -> Result<ExecutionReadyJob> {
    let Some(repo_full_name) = job.repo.clone() else {
        return Ok(no_work());
    };

    let Some((owner, repo_name)) = repo_full_name.split_once('/') else {
        return Err(worker::Error::RustError("queued job has invalid repo name".into()));
    };

    let mut repo_response =
        github_request(ctx, &format!("/repos/{owner}/{repo_name}"), Method::Get, None, &[200])
            .await?;
    let repo_payload: Value = repo_response.json().await?;
    let default_branch = repo_payload["default_branch"].as_str().unwrap_or("main");
    let repo_clone_url = repo_payload["clone_url"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| format!("https://github.com/{repo_full_name}.git"));

    let mut token_response = github_request(
        ctx,
        &format!("/repos/{owner}/{repo_name}/actions/runners/registration-token"),
        Method::Post,
        Some("{}".to_string()),
        &[201],
    )
    .await?;
    let token_payload: Value = token_response.json().await?;

    let mut cache_response = github_request(
        ctx,
        &format!("/repos/{owner}/{repo_name}/contents/Cargo.lock?ref={default_branch}"),
        Method::Get,
        None,
        &[200, 404],
    )
    .await?;
    let cache_key = if cache_response.status_code() == 404 {
        None
    } else {
        let cache_payload: Value = cache_response.json().await?;
        let encoded = cache_payload["content"]
            .as_str()
            .unwrap_or_default()
            .replace('\n', "");
        let decoded = BASE64_STANDARD
            .decode(encoded)
            .map_err(|error| worker::Error::RustError(error.to_string()))?;
        let mut hasher = Sha256::new();
        hasher.update(b"cargo-lock:");
        hasher.update(decoded);
        Some(hex::encode(hasher.finalize()))
    };

    Ok(ExecutionReadyJob {
        job_id: job.job_id,
        run_id: job.run_id,
        repo_full_name: Some(repo_full_name.clone()),
        repo_clone_url: Some(repo_clone_url),
        labels: job.labels,
        runner_registration_url: Some(format!("https://github.com/{repo_full_name}")),
        runner_registration_token: token_payload["token"].as_str().map(str::to_string),
        runner_registration_expires_at: token_payload["expires_at"].as_str().map(str::to_string),
        cache_key,
    })
}

pub async fn peek(req: Request, ctx: RouteContext<AppState>) -> Result<Response> {
    if let Err(worker::Error::RustError(message)) = require_internal_token(&req, &ctx) {
        return Response::error(&message, 401);
    }

    let job = read_queue(&ctx, "https://job-queue/peek").await?;
    let prepared = prepare_job(job, &ctx).await?;

    Response::from_json(&prepared)
}

pub async fn next(req: Request, ctx: RouteContext<AppState>) -> Result<Response> {
    if let Err(worker::Error::RustError(message)) = require_internal_token(&req, &ctx) {
        return Response::error(&message, 401);
    }

    let job = read_queue(&ctx, "https://job-queue/dequeue").await?;
    let prepared = prepare_job(job, &ctx).await?;

    if let Some(job_id) = prepared.job_id {
        console_log!("action=job_dequeued job_id={}", job_id);
    }

    Response::from_json(&prepared)
}
