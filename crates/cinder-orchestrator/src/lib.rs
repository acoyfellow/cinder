use worker::*;

mod build;
mod cache;
mod job_queue_do;
mod jobs;
mod runner_pool_do;
mod runners;
mod webhook;

#[derive(Clone)]
pub struct AppState {
    pub internal_token: String,
    pub webhook_secret: String,
}

#[event(fetch)]
pub async fn main(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    let state = AppState {
        internal_token: env.secret("CINDER_INTERNAL_TOKEN")?.to_string(),
        webhook_secret: env.secret("GITHUB_WEBHOOK_SECRET")?.to_string(),
    };

    let router = Router::with_data(state);

    router
        // public — github calls this
        .post_async("/webhook/github", webhook::handle)
        // internal — agents call these
        .get_async("/jobs/next", jobs::next)
        .post_async("/runners/register", runners::register)
        .delete_async("/runners/:id", runners::deregister)
        // cache — agents get presigned URLs from here
        .post_async("/cache/restore/:key", cache::restore)
        .post_async("/cache/upload", cache::upload)
        .post_async("/test/build", build::run)
        // admin
        .post_async("/internal/token/rotate", |_, _| async {
            Response::error("not implemented", 501)
        })
        .run(req, env)
        .await
}
