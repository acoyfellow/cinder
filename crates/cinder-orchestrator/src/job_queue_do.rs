use wasm_bindgen::prelude::wasm_bindgen;
use worker::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QueuedJob {
    pub job_id: Option<u64>,
    pub run_id: Option<u64>,
    pub repo: Option<String>,
    pub labels: Vec<String>,
}

#[durable_object]
pub struct JobQueue {
    #[allow(dead_code)]
    state: State,
    #[allow(dead_code)]
    env: Env,
}

#[durable_object]
impl DurableObject for JobQueue {
    fn new(state: State, env: Env) -> Self {
        Self { state, env }
    }

    async fn fetch(&mut self, mut req: Request) -> Result<Response> {
        match (req.method(), req.path().as_str()) {
            (Method::Post, "/enqueue") => {
                let job: QueuedJob = req.json().await?;
                self.state.storage().put("next_job", &job).await?;
                Response::ok("queued")
            }
            (Method::Post, "/evict") => {
                let candidate: QueuedJob = req.json().await?;
                if let Ok(current) = self.state.storage().get::<QueuedJob>("next_job").await {
                    if current.job_id == candidate.job_id && current.run_id == candidate.run_id {
                        self.state.storage().delete("next_job").await?;
                    }
                }

                Response::ok("evicted")
            }
            (Method::Get, "/dequeue") => {
                let job = match self.state.storage().get::<QueuedJob>("next_job").await {
                    Ok(job) => {
                        self.state.storage().delete("next_job").await?;
                        job
                    }
                    Err(_) => QueuedJob {
                        job_id: None,
                        run_id: None,
                        repo: None,
                        labels: Vec::new(),
                    },
                };

                Response::from_json(&job)
            }
            (Method::Get, "/peek") => {
                let job = match self.state.storage().get::<QueuedJob>("next_job").await {
                    Ok(job) => job,
                    Err(_) => QueuedJob {
                        job_id: None,
                        run_id: None,
                        repo: None,
                        labels: Vec::new(),
                    },
                };

                Response::from_json(&job)
            }
            _ => Response::error("not found", 404),
        }
    }
}
