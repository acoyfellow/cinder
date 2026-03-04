use hmac::{Hmac, Mac};
use sha2::Sha256;
use worker::*;

type HmacSha256 = Hmac<Sha256>;

fn query_param(url: &Url, name: &str) -> std::result::Result<String, Response> {
    url.query_pairs()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.into_owned())
        .ok_or_else(|| Response::error(format!("missing {name}"), 400).unwrap())
}

fn authorize(req: &Request, key: &str, expected_op: &str, secret: &str) -> std::result::Result<(), Response> {
    let url = req
        .url()
        .map_err(|err| Response::error(err.to_string(), 400).unwrap())?;
    let op = query_param(&url, "op")?;

    if op != expected_op {
        return Err(Response::error("wrong operation", 405).unwrap());
    }

    let exp = query_param(&url, "exp")?;
    let exp_seconds = exp
        .parse::<u64>()
        .map_err(|_| Response::error("invalid exp", 400).unwrap())?;

    if exp_seconds <= Date::now().as_millis() / 1000 {
        return Err(Response::error("expired", 403).unwrap());
    }

    let sig = query_param(&url, "sig")?;
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .map_err(|err| Response::error(err.to_string(), 500).unwrap())?;
    mac.update(format!("{expected_op}:{key}:{exp}").as_bytes());
    let expected_sig = hex::encode(mac.finalize().into_bytes());

    if sig != expected_sig {
        return Err(Response::error("invalid signature", 403).unwrap());
    }

    Ok(())
}

#[event(fetch)]
pub async fn main(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    let secret = env.secret("CINDER_INTERNAL_TOKEN")?.to_string();
    let bucket = env.bucket("CACHE_BUCKET")?;
    let get_secret = secret.clone();
    let get_bucket = bucket.clone();
    let put_secret = secret.clone();
    let put_bucket = bucket.clone();
    let delete_secret = secret.clone();
    let delete_bucket = bucket.clone();
    let router = Router::new();

    router
        .get_async("/health", |_, _| async { Response::ok("ok") })
        .get_async("/objects/:key", move |req, ctx| {
            let secret = get_secret.clone();
            let bucket = get_bucket.clone();
            async move {
                let key = ctx.param("key").map_or("", |value| value.as_str()).to_string();

                if let Err(response) = authorize(&req, &key, "get", &secret) {
                    return Ok(response);
                }

                let object = match bucket.get(&key).execute().await? {
                    Some(object) => object,
                    None => return Response::error("not found", 404),
                };

                let response = match object.body() {
                    Some(body) => Response::from_body(body.response_body()?)?,
                    None => Response::empty()?,
                };

                Ok(response)
            }
        })
        .put_async("/objects/:key", move |mut req, ctx| {
            let secret = put_secret.clone();
            let bucket = put_bucket.clone();
            async move {
                let key = ctx.param("key").map_or("", |value| value.as_str()).to_string();

                if let Err(response) = authorize(&req, &key, "put", &secret) {
                    return Ok(response);
                }

                let body = req.bytes().await?;
                bucket.put(&key, body).execute().await?;

                Response::ok("ok")
            }
        })
        .delete_async("/objects/:key", move |req, ctx| {
            let secret = delete_secret.clone();
            let bucket = delete_bucket.clone();
            async move {
                let key = ctx.param("key").map_or("", |value| value.as_str()).to_string();

                if let Err(response) = authorize(&req, &key, "delete", &secret) {
                    return Ok(response);
                }

                bucket.delete(&key).await?;
                Response::ok("ok")
            }
        })
        .run(req, env)
        .await
}
