use crate::AppState;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use worker::*;

type HmacSha256 = Hmac<Sha256>;

fn signed_cache_url(ctx: &RouteContext<AppState>, key: &str, op: &str) -> Result<String> {
    let exp = (Date::now().as_millis() / 1000) + 3600;
    let message = format!("{op}:{key}:{exp}");
    let mut mac = HmacSha256::new_from_slice(ctx.data.internal_token.as_bytes())
        .map_err(|err| Error::RustError(err.to_string()))?;
    mac.update(message.as_bytes());
    let sig = hex::encode(mac.finalize().into_bytes());
    let base = ctx.data.cache_worker_url.trim_end_matches('/');

    Ok(format!("{base}/objects/{key}?op={op}&exp={exp}&sig={sig}"))
}

pub async fn restore(_req: Request, ctx: RouteContext<AppState>) -> Result<Response> {
    let key = ctx.param("key").map_or("", |value| value.as_str()).to_string();
    let bucket = ctx.env.bucket("CACHE_BUCKET")?;

    match bucket.head(&key).await? {
        Some(_) => {
            console_log!("action=cache_hit key={}", key);
            Response::from_json(&serde_json::json!({
                "miss": false,
                "url": signed_cache_url(&ctx, &key, "get")?,
                "expires_in": 3600,
            }))
        }
        None => {
            console_log!("action=cache_miss key={}", key);
            Response::from_json(&serde_json::json!({
                "miss": true,
                "url": null,
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

    console_log!("action=upload_url_generated key={}", key);

    Response::from_json(&serde_json::json!({
        "url": signed_cache_url(&ctx, key, "put")?,
        "method": "PUT",
        "expires_in": 3600,
    }))
}
