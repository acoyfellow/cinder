use wasm_bindgen::prelude::wasm_bindgen;
use worker::*;

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

    async fn fetch(&mut self, _req: Request) -> Result<Response> {
        Response::error("RunnerPool not implemented", 501)
    }
}
