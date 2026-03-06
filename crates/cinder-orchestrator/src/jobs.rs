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

enum QueuedJobState {
    Empty,
    Runnable(QueuedJob),
    Stale { job: QueuedJob, reason: String },
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

async fn post_queue(ctx: &RouteContext<AppState>, path: &str, job: &QueuedJob) -> Result<Response> {
    let queue = ctx.env.durable_object("JOB_QUEUE")?;
    let queue_id = queue.id_from_name("default")?;
    let stub = queue_id.get_stub()?;
    let mut init = RequestInit::new();
    init.with_method(Method::Post);
    init.with_body(Some(JsValue::from_str(
        &serde_json::to_string(job)
            .map_err(|error| worker::Error::RustError(error.to_string()))?,
    )));
    let mut request = Request::new_with_init(path, &init)?;
    request.headers_mut()?.set("Content-Type", "application/json")?;
    stub.fetch_with_request(request).await
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

async fn classify_queued_job(job: QueuedJob, ctx: &RouteContext<AppState>) -> Result<QueuedJobState> {
    let Some(repo_full_name) = job.repo.clone() else {
        return Ok(QueuedJobState::Empty);
    };

    let Some(run_id) = job.run_id else {
        return Ok(QueuedJobState::Runnable(job));
    };

    let Some((owner, repo_name)) = repo_full_name.split_once('/') else {
        return Err(worker::Error::RustError("queued job has invalid repo name".into()));
    };

    let mut run_response = github_request(
        ctx,
        &format!("/repos/{owner}/{repo_name}/actions/runs/{run_id}"),
        Method::Get,
        None,
        &[200, 404],
    )
    .await?;

    if run_response.status_code() == 404 {
        return Ok(QueuedJobState::Stale {
            job,
            reason: "run_missing".into(),
        });
    }

    let run_payload: Value = run_response.json().await?;
    let run_status = run_payload["status"].as_str().unwrap_or_default();
    let run_conclusion = run_payload["conclusion"].as_str();

    if run_status == "completed" {
        let reason = run_conclusion
            .map(|conclusion| format!("run_completed:{conclusion}"))
            .unwrap_or_else(|| "run_completed".into());
        return Ok(QueuedJobState::Stale { job, reason });
    }

    if let Some(job_id) = job.job_id {
        let mut jobs_response = github_request(
            ctx,
            &format!("/repos/{owner}/{repo_name}/actions/runs/{run_id}/jobs?per_page=100"),
            Method::Get,
            None,
            &[200],
        )
        .await?;
        let jobs_payload: Value = jobs_response.json().await?;
        let jobs = jobs_payload["jobs"].as_array().cloned().unwrap_or_default();
        let maybe_job_payload = jobs
            .into_iter()
            .find(|candidate| candidate["id"].as_u64() == Some(job_id));

        if let Some(job_payload) = maybe_job_payload {
            let job_status = job_payload["status"].as_str().unwrap_or_default();
            let job_conclusion = job_payload["conclusion"].as_str();
            if job_status == "completed" {
                let reason = job_conclusion
                    .map(|conclusion| format!("job_completed:{conclusion}"))
                    .unwrap_or_else(|| "job_completed".into());
                return Ok(QueuedJobState::Stale { job, reason });
            }
        }
    }

    let workflow_id = run_payload["workflow_id"].as_u64();
    let head_branch = run_payload["head_branch"].as_str();
    if let (Some(workflow_id), Some(head_branch)) = (workflow_id, head_branch) {
        let mut newer_runs_response = github_request(
            ctx,
            &format!(
                "/repos/{owner}/{repo_name}/actions/workflows/{workflow_id}/runs?branch={head_branch}&per_page=20"
            ),
            Method::Get,
            None,
            &[200],
        )
        .await?;
        let newer_runs_payload: Value = newer_runs_response.json().await?;
        let newer_run = newer_runs_payload["workflow_runs"]
            .as_array()
            .and_then(|runs| {
                runs.iter().find(|candidate| {
                    candidate["id"].as_u64().is_some_and(|candidate_id| {
                        candidate_id > run_id
                            && candidate["status"].as_str().unwrap_or_default() != "completed"
                    })
                })
            })
            .and_then(|candidate| candidate["id"].as_u64());

        if let Some(newer_run_id) = newer_run {
            return Ok(QueuedJobState::Stale {
                job,
                reason: format!("superseded_by_run:{newer_run_id}"),
            });
        }
    }

    Ok(QueuedJobState::Runnable(job))
}

async fn evict_if_current(ctx: &RouteContext<AppState>, job: &QueuedJob) -> Result<()> {
    let response = post_queue(ctx, "https://job-queue/evict", job).await?;
    if response.status_code() >= 400 {
        return Err(worker::Error::RustError("failed to evict stale job".into()));
    }

    Ok(())
}

async fn resolve_runnable_job(ctx: &RouteContext<AppState>, destructive: bool) -> Result<QueuedJob> {
    for _ in 0..8 {
        let job = if destructive {
            read_queue(ctx, "https://job-queue/dequeue").await?
        } else {
            read_queue(ctx, "https://job-queue/peek").await?
        };

        match classify_queued_job(job, ctx).await? {
            QueuedJobState::Empty => {
                return Ok(QueuedJob {
                    job_id: None,
                    run_id: None,
                    repo: None,
                    labels: Vec::new(),
                });
            }
            QueuedJobState::Runnable(job) => return Ok(job),
            QueuedJobState::Stale { job, reason } => {
                if let Some(job_id) = job.job_id {
                    let run_id = job.run_id.unwrap_or_default();
                    console_log!(
                        "action=stale_job_skipped job_id={} run_id={} reason={}",
                        job_id,
                        run_id,
                        reason
                    );
                }
                if !destructive {
                    evict_if_current(ctx, &job).await?;
                }
            }
        }
    }

    Err(worker::Error::RustError(
        "job queue could not resolve a runnable job after stale eviction".into(),
    ))
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

    let job = resolve_runnable_job(&ctx, false).await?;
    let prepared = prepare_job(job, &ctx).await?;

    if let Some(job_id) = prepared.job_id {
        let run_id = prepared.run_id.unwrap_or_default();
        console_log!("action=deploy_job_selected job_id={} run_id={}", job_id, run_id);
    }

    Response::from_json(&prepared)
}

pub async fn next(req: Request, ctx: RouteContext<AppState>) -> Result<Response> {
    if let Err(worker::Error::RustError(message)) = require_internal_token(&req, &ctx) {
        return Response::error(&message, 401);
    }

    let job = resolve_runnable_job(&ctx, true).await?;
    let prepared = prepare_job(job, &ctx).await?;

    if let Some(job_id) = prepared.job_id {
        let run_id = prepared.run_id.unwrap_or_default();
        console_log!("action=job_dequeued job_id={}", job_id);
        console_log!("action=deploy_job_selected job_id={} run_id={}", job_id, run_id);
    }

    Response::from_json(&prepared)
}
