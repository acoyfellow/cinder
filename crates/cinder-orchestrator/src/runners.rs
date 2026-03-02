use worker::*;
use crate::AppState;

pub async fn register(mut req: Request, ctx: RouteContext<AppState>) -> Result<Response> {
    let token = req
        .headers()
        .get("Authorization")?
        .ok_or_else(|| worker::Error::RustError("missing auth".into()))?;

    if token != format!("Bearer {}", ctx.data.internal_token) {
        return Response::error("unauthorized", 401);
    }

    let body: serde_json::Value = req.json().await?;
    let runner_id = body["runner_id"].as_str().unwrap_or("unknown");

    // TODO: route to RunnerPool DO and register
    console_log!("action=runner_registered runner_id={}", runner_id);
    console_log!("action=runner_pool_updated");

    Response::ok("registered")
}

pub async fn deregister(_req: Request, ctx: RouteContext<AppState>) -> Result<Response> {
    let id = ctx.param("id").map_or("unknown", |value| value.as_str());

    // TODO: route to RunnerPool DO and deregister
    console_log!("action=runner_deregistered runner_id={}", id);

    Response::ok("deregistered")
}
