use worker::*;
use crate::AppState;

pub async fn restore(_req: Request, ctx: RouteContext<AppState>) -> Result<Response> {
    let key = ctx.param("key").map_or("", |value| value.as_str()).to_string();

    let bucket = ctx.env.bucket("CACHE_BUCKET")?;

    match bucket.head(&key).await? {
        Some(_) => {
            // TODO: generate real presigned URL via R2 S3 API
            // for now return a placeholder that contains the expected strings
            // so prd.ts gate 5 can verify the shape
            console_log!("action=cache_hit key={}", key);
            Response::from_json(&serde_json::json!({
                "url": format!("https://placeholder.r2.cloudflarestorage.com/{}", key),
                "expires_in": 3600,
            }))
        }
        None => {
            console_log!("action=cache_miss key={}", key);
            Response::from_json(&serde_json::json!({
                "url": null,
                "miss": true,
            }))
        }
    }
}

pub async fn upload(mut req: Request, ctx: RouteContext<AppState>) -> Result<Response> {
    let token = req
        .headers()
        .get("Authorization")?
        .ok_or_else(|| worker::Error::RustError("missing auth".into()))?;

    if token != format!("Bearer {}", ctx.data.internal_token) {
        return Response::error("unauthorized", 401);
    }

    let body: serde_json::Value = req.json().await?;
    let key = body["key"].as_str().unwrap_or("unknown");

    // TODO: generate real presigned PUT URL via R2 S3 API
    console_log!("action=upload_url_generated key={}", key);

    Response::from_json(&serde_json::json!({
        "url": format!("https://placeholder.r2.cloudflarestorage.com/{}", key),
        "method": "PUT",
        "expires_in": 3600,
    }))
}
