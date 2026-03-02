use wasm_bindgen::prelude::wasm_bindgen;
use worker::*;

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

    async fn fetch(&mut self, _req: Request) -> Result<Response> {
        Response::error("JobQueue not implemented", 501)
    }
}
