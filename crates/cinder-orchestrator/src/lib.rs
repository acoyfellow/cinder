use worker::*;

mod build;
mod cache;
mod job_queue_do;
mod jobs;
mod repos;
mod runner_pool_do;
mod runners;
mod webhook;

#[derive(Clone)]
pub struct AppState {
    pub internal_token: String,
    pub webhook_secret: String,
    pub github_pat: String,
    pub cache_worker_url: String,
    pub fixture_repo: String,
    pub fixture_branch: String,
    pub fixture_workflow: String,
}

#[event(fetch)]
pub async fn main(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    let state = AppState {
        internal_token: env.secret("CINDER_INTERNAL_TOKEN")?.to_string(),
        webhook_secret: env.secret("GITHUB_WEBHOOK_SECRET")?.to_string(),
        github_pat: env.secret("GITHUB_PAT")?.to_string(),
        cache_worker_url: env.var("CINDER_CACHE_WORKER_URL")?.to_string(),
        fixture_repo: env.var("CINDER_FIXTURE_REPO")?.to_string(),
        fixture_branch: env.var("CINDER_FIXTURE_BRANCH")?.to_string(),
        fixture_workflow: env.var("CINDER_FIXTURE_WORKFLOW")?.to_string(),
    };

    let router = Router::with_data(state);

    router
        // public — github calls this
        .post_async("/webhook/github", webhook::handle)
        // internal — agents call these
        .post_async("/repos/connect", repos::connect)
        .get_async("/repos", repos::list)
        .post_async("/repos/:owner/:repo/dispatches", repos::dispatch)
        .post_async("/repos/:owner/:repo/proof-runs", repos::proof_run_create)
        .get_async("/repos/:owner/:repo/state", repos::state)
        .get_async("/jobs/peek", jobs::peek)
        .get_async("/jobs/next", jobs::next)
        .post_async("/runners/register", runners::register)
        .delete_async("/runners/:id", runners::deregister)
        // cache — agents get presigned URLs from here
        .post_async("/cache/restore/:key", cache::restore)
        .post_async("/cache/upload", cache::upload)
        .post_async("/test/build", build::run)
        // proof runs
        .get_async("/proof-runs/:id", repos::proof_run_show)
        // admin
        .post_async("/internal/token/rotate", |_, _| async {
            Response::error("not implemented", 501)
        })
        .run(req, env)
        .await
}
