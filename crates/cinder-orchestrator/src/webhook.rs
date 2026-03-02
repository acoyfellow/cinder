use worker::*;
use crate::AppState;
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

pub async fn handle(mut req: Request, ctx: RouteContext<AppState>) -> Result<Response> {
    let body = req.bytes().await?;

    // verify github signature
    let sig = req
        .headers()
        .get("X-Hub-Signature-256")?
        .ok_or_else(|| worker::Error::RustError("missing signature".into()))?;

    let expected = sign(&ctx.data.webhook_secret, &body);
    if !constant_time_eq(sig.as_bytes(), expected.as_bytes()) {
        return Response::error("invalid signature", 401);
    }

    console_log!("action=webhook_received");
    console_log!("action=signature_verified");

    let payload: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|e| worker::Error::RustError(e.to_string()))?;

    let action = payload["action"].as_str().unwrap_or("");

    if action == "queued" {
        let job_id = payload["workflow_job"]["id"].as_u64().unwrap_or(0);
        let _queue = ctx.env.durable_object("JOB_QUEUE")?;
        // TODO: route to JobQueue DO and enqueue
        console_log!("action=job_queued job_id={}", job_id);
    }

    Response::ok("ok")
}

fn sign(secret: &str, body: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .expect("hmac accepts any key size");
    mac.update(body);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b.iter()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}
