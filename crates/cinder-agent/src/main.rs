use anyhow::Result;
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::{info, warn};

#[derive(Parser, Debug)]
#[command(name = "cinder-agent", about = "cinder runner agent")]
struct Args {
    #[arg(long, env = "CINDER_URL")]
    url: String,

    #[arg(long, env = "CINDER_TOKEN")]
    token: String,

    #[arg(long, default_value = "self-hosted,cinder", env = "CINDER_LABELS")]
    labels: String,

    #[arg(long, default_value = "1000")]
    poll_ms: u64,

    #[arg(long, default_value = "/tmp/cinder")]
    cache_dir: PathBuf,
}

#[derive(Debug, Serialize)]
struct RegisterRequest {
    runner_id: String,
    labels: Vec<String>,
    arch: String,
}

#[derive(Debug, Deserialize)]
struct Job {
    job_id: Option<u64>,
    run_id: Option<u64>,
    repo: Option<String>,
    labels: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let args = Args::parse();
    let client = reqwest::Client::new();
    let runner_id = format!("cinder-{}", hostname());

    // register
    info!("registering runner {}", runner_id);
    client
        .post(format!("{}/runners/register", args.url))
        .bearer_auth(&args.token)
        .json(&RegisterRequest {
            runner_id: runner_id.clone(),
            labels: args.labels.split(',').map(String::from).collect(),
            arch: std::env::consts::ARCH.to_string(),
        })
        .send()
        .await?
        .error_for_status()?;

    info!("registered. polling for jobs every {}ms", args.poll_ms);

    // poll loop
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(args.poll_ms)).await;

        let resp = client
            .get(format!("{}/jobs/next", args.url))
            .bearer_auth(&args.token)
            .send()
            .await?;

        if !resp.status().is_success() {
            warn!("poll failed: {}", resp.status());
            continue;
        }

        let job: Job = resp.json().await?;

        if job.job_id.is_none() {
            continue; // no work
        }

        info!(
            "got job {} for repo {:?}",
            job.job_id.unwrap(),
            job.repo
        );

        // TODO: restore cache, inject env vars, run github actions runner,
        // push cache diff on completion
    }
}

fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .unwrap_or_else(|_| "unknown".into())
        .trim()
        .to_string()
}
