use worker::*;
use crate::AppState;
use crate::job_queue_do::QueuedJob;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use wasm_bindgen::JsValue;

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
        let run_id = payload["workflow_job"]["run_id"].as_u64();
        let repo = payload["repository"]["full_name"].as_str().map(str::to_string);
        let labels = payload["workflow_job"]["labels"]
            .as_array()
            .map(|values| {
                values
                    .iter()
                    .filter_map(|value| value.as_str().map(str::to_string))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let queue = ctx.env.durable_object("JOB_QUEUE")?;
        let queue_id = queue.id_from_name("default")?;
        let stub = queue_id.get_stub()?;
        let mut request_init = RequestInit::new();
        request_init.with_method(Method::Post);
        request_init.with_body(Some(JsValue::from_str(
            &serde_json::to_string(&QueuedJob {
                job_id: Some(job_id),
                run_id,
                repo,
                labels,
            })
            .map_err(|error| worker::Error::RustError(error.to_string()))?,
        )));
        let mut queue_request = Request::new_with_init("https://job-queue/enqueue", &request_init)?;
        queue_request
            .headers_mut()?
            .set("Content-Type", "application/json")?;
        let queue_response = stub.fetch_with_request(queue_request).await?;
        if queue_response.status_code() >= 400 {
            return Response::error("failed to enqueue job", 500);
        }
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
