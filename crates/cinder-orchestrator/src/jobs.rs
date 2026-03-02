use worker::*;
use crate::AppState;

pub async fn next(req: Request, ctx: RouteContext<AppState>) -> Result<Response> {
    let token = req
        .headers()
        .get("Authorization")?
        .ok_or_else(|| worker::Error::RustError("missing auth".into()))?;

    if token != format!("Bearer {}", ctx.data.internal_token) {
        return Response::error("unauthorized", 401);
    }

    // TODO: route to JobQueue DO and dequeue next job
    console_log!("action=job_dequeued");

    Response::from_json(&serde_json::json!({
        "job_id": null,
        "run_id": null,
        "repo": null,
        "labels": [],
    }))
}
