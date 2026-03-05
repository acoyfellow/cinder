use wasm_bindgen::prelude::wasm_bindgen;
use serde::{Deserialize, Serialize};
use worker::*;

const RUNNERS_KEY: &str = "runners";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunnerRecord {
    pub runner_id: String,
    pub labels: Vec<String>,
    pub arch: String,
}

#[durable_object]
pub struct RunnerPool {
    #[allow(dead_code)]
    state: State,
    #[allow(dead_code)]
    env: Env,
}

#[durable_object]
impl DurableObject for RunnerPool {
    fn new(state: State, env: Env) -> Self {
        Self { state, env }
    }

    async fn fetch(&mut self, mut req: Request) -> Result<Response> {
        match (req.method(), req.path().as_str()) {
            (Method::Post, "/register") => {
                let runner: RunnerRecord = req.json().await?;
                let mut runners = load_runners(&self.state).await?;

                runners.retain(|current| current.runner_id != runner.runner_id);
                runners.push(runner);

                save_runners(&self.state, &runners).await?;

                Response::ok("registered")
            }
            (Method::Get, "/runners") => {
                let runners = load_runners(&self.state).await?;
                Response::from_json(&runners)
            }
            (Method::Delete, path) if path.starts_with("/runners/") => {
                let runner_id = path.trim_start_matches("/runners/");
                let mut runners = load_runners(&self.state).await?;
                let before = runners.len();

                runners.retain(|current| current.runner_id != runner_id);
                save_runners(&self.state, &runners).await?;

                if runners.len() == before {
                    return Response::error("runner not found", 404);
                }

                Response::ok("deregistered")
            }
            _ => Response::error("not found", 404),
        }
    }
}

async fn load_runners(state: &State) -> Result<Vec<RunnerRecord>> {
    match state.storage().get::<Vec<RunnerRecord>>(RUNNERS_KEY).await {
        Ok(runners) => Ok(runners),
        Err(_) => Ok(Vec::new()),
    }
}

async fn save_runners(state: &State, runners: &[RunnerRecord]) -> Result<()> {
    state.storage().put(RUNNERS_KEY, runners).await
}
