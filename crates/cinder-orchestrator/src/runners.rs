use worker::*;
use crate::AppState;
use crate::runner_pool_do::RunnerRecord;

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

pub async fn register(mut req: Request, ctx: RouteContext<AppState>) -> Result<Response> {
    if let Err(worker::Error::RustError(message)) = require_internal_token(&req, &ctx) {
        return Response::error(&message, 401);
    }

    let runner: RunnerRecord = req.json().await?;
    let runner_id = runner.runner_id.clone();

    let pool = ctx.env.durable_object("RUNNER_POOL")?;
    let pool_id = pool.id_from_name("default")?;
    let stub = pool_id.get_stub()?;

    let mut pool_request = Request::new_with_init(
        "https://runner-pool/register",
        RequestInit::new()
            .with_method(Method::Post)
            .with_body(Some(serde_json::to_string(&runner)?.into())),
    )?;
    pool_request
        .headers_mut()?
        .set("Content-Type", "application/json")?;
    let pool_response = stub.fetch_with_request(pool_request).await?;

    if pool_response.status_code() >= 400 {
        return Response::error("failed to register runner", 500);
    }

    console_log!("action=runner_registered runner_id={}", runner_id);
    console_log!("action=runner_pool_updated");

    Response::ok("registered")
}

pub async fn deregister(req: Request, ctx: RouteContext<AppState>) -> Result<Response> {
    if let Err(worker::Error::RustError(message)) = require_internal_token(&req, &ctx) {
        return Response::error(&message, 401);
    }

    let id = ctx.param("id").map_or("unknown", |value| value.as_str());

    let pool = ctx.env.durable_object("RUNNER_POOL")?;
    let pool_id = pool.id_from_name("default")?;
    let stub = pool_id.get_stub()?;
    let pool_request = Request::new_with_init(
        &format!("https://runner-pool/runners/{id}"),
        RequestInit::new().with_method(Method::Delete),
    )?;
    let pool_response = stub.fetch_with_request(pool_request).await?;

    if pool_response.status_code() >= 400 {
        return Response::error("failed to deregister runner", 500);
    }

    console_log!("action=runner_deregistered runner_id={}", id);

    Response::ok("deregistered")
}
