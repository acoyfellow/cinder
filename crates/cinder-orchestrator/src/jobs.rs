use worker::*;
use crate::AppState;
use crate::job_queue_do::QueuedJob;

pub async fn next(req: Request, ctx: RouteContext<AppState>) -> Result<Response> {
    let token = req
        .headers()
        .get("Authorization")?
        .ok_or_else(|| worker::Error::RustError("missing auth".into()))?;

    if token != format!("Bearer {}", ctx.data.internal_token) {
        return Response::error("unauthorized", 401);
    }

    let queue = ctx.env.durable_object("JOB_QUEUE")?;
    let queue_id = queue.id_from_name("default")?;
    let stub = queue_id.get_stub()?;
    let queue_response = stub.fetch_with_str("https://job-queue/dequeue").await?;
    if queue_response.status_code() >= 400 {
        return Response::error("failed to dequeue job", 500);
    }

    let mut queue_response = queue_response;
    let job: QueuedJob = queue_response.json().await?;
    if let Some(job_id) = job.job_id {
        console_log!("action=job_dequeued job_id={}", job_id);
    }

    Response::from_json(&job)
}
