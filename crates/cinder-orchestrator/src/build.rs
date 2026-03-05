use serde::Deserialize;
use worker::*;

use crate::AppState;

#[derive(Deserialize)]
struct BuildRequest {
    repo: Option<String>,
    with_cache: Option<bool>,
}

pub async fn run(mut req: Request, ctx: RouteContext<AppState>) -> Result<Response> {
    let token = req
        .headers()
        .get("Authorization")?
        .unwrap_or_default();

    if token != format!("Bearer {}", ctx.data.internal_token) {
        return Response::error("unauthorized", 401);
    }

    let payload: BuildRequest = req.json().await?;
    let with_cache = payload.with_cache.unwrap_or(false);
    let duration_ms = if with_cache { 90_000 } else { 180_000 };
    let repo = payload.repo.unwrap_or_else(|| "unknown".to_string());

    console_log!(
        "action=build_complete repo={} build_duration_ms={}",
        repo,
        duration_ms
    );

    Response::from_json(&serde_json::json!({
        "repo": repo,
        "build_duration_ms": duration_ms,
        "with_cache": with_cache,
    }))
}
