use anyhow::{anyhow, bail, Context, Result};
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::{fs, process::Command};
use tracing::{error, info, warn};

const RUNNER_VERSION: &str = "2.328.0";

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
    repo_full_name: Option<String>,
    repo_clone_url: Option<String>,
    labels: Vec<String>,
    runner_registration_url: Option<String>,
    runner_registration_token: Option<String>,
    runner_registration_expires_at: Option<String>,
    cache_key: Option<String>,
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

        let job_id = job.job_id.unwrap();
        let run_id = job.run_id;
        let repo_full_name = job.repo_full_name.clone();

        info!("got job {} for repo {:?}", job_id, repo_full_name);

        if let Err(error) = execute_job(&client, &args, &runner_id, &job).await {
            error!("job {} failed: {error:#}", job_id);
            if let Some(run_id) = run_id {
                warn!("job {} (run {}) did not complete successfully", job_id, run_id);
            }
        }
    }
}

async fn execute_job(
    client: &reqwest::Client,
    args: &Args,
    runner_id: &str,
    job: &Job,
) -> Result<()> {
    let job_id = job.job_id.context("missing job_id in execution payload")?;
    let repo_full_name = job
        .repo_full_name
        .as_deref()
        .context("missing repo_full_name in execution payload")?;
    let runner_registration_url = job
        .runner_registration_url
        .as_deref()
        .context("missing runner_registration_url in execution payload")?;
    let runner_registration_token = job
        .runner_registration_token
        .as_deref()
        .context("missing runner_registration_token in execution payload")?;

    if let Some(repo_clone_url) = job.repo_clone_url.as_deref() {
        info!("job {} clone url {}", job_id, repo_clone_url);
    }

    if let Some(expires_at) = job.runner_registration_expires_at.as_deref() {
        info!("job {} runner token expires at {}", job_id, expires_at);
    }

    if let Some(cache_key) = job.cache_key.as_deref() {
        info!("job {} cache key {}", job_id, cache_key);
    }

    let labels = if job.labels.is_empty() {
        vec!["self-hosted".to_string(), "cinder".to_string()]
    } else {
        job.labels.clone()
    };

    let jobs_dir = args.cache_dir.join("jobs");
    let job_dir = jobs_dir.join(job_id.to_string());
    let toolcache_dir = args.cache_dir.join("runner-toolcache");
    let archive_path = ensure_runner_archive(client, &toolcache_dir).await?;

    if fs::try_exists(&job_dir).await.unwrap_or(false) {
        fs::remove_dir_all(&job_dir)
            .await
            .with_context(|| format!("remove stale job directory {}", job_dir.display()))?;
    }

    fs::create_dir_all(&jobs_dir)
        .await
        .with_context(|| format!("create jobs directory {}", jobs_dir.display()))?;
    fs::create_dir_all(&job_dir)
        .await
        .with_context(|| format!("create job directory {}", job_dir.display()))?;

    extract_runner_archive(&archive_path, &job_dir).await?;

    println!("starting github runner for job {}", job_id);
    info!("starting github runner for job {}", job_id);

    let runner_name = format!("{runner_id}-{job_id}");
    configure_runner(
        &job_dir,
        runner_registration_url,
        runner_registration_token,
        &runner_name,
        &labels,
    )
    .await?;

    println!("github runner configured for {}", repo_full_name);
    info!("github runner configured for {}", repo_full_name);

    let status = Command::new("./run.sh")
        .current_dir(&job_dir)
        .status()
        .await
        .context("start github runner")?;
    let exit_code = status.code().unwrap_or(-1);

    println!("job {} completed with exit code {}", job_id, exit_code);
    info!("job {} completed with exit code {}", job_id, exit_code);

    if status.success() {
        fs::remove_dir_all(&job_dir)
            .await
            .with_context(|| format!("remove completed job directory {}", job_dir.display()))?;
        return Ok(());
    }

    warn!(
        "preserving failed job {} work directory at {}",
        job_id,
        job_dir.display()
    );
    bail!("github runner exited with {exit_code}");
}

async fn ensure_runner_archive(client: &reqwest::Client, toolcache_dir: &PathBuf) -> Result<PathBuf> {
    fs::create_dir_all(toolcache_dir)
        .await
        .with_context(|| format!("create runner toolcache {}", toolcache_dir.display()))?;

    let artifact = runner_artifact_name()?;
    let archive_path = toolcache_dir.join(artifact);

    if fs::try_exists(&archive_path).await.unwrap_or(false) {
        return Ok(archive_path);
    }

    let download_url = format!(
        "https://github.com/actions/runner/releases/download/v{RUNNER_VERSION}/{artifact}"
    );
    info!("downloading github runner {}", artifact);

    let bytes = client
        .get(download_url)
        .send()
        .await
        .context("download github runner")?
        .error_for_status()
        .context("github runner download failed")?
        .bytes()
        .await
        .context("read github runner archive")?;

    fs::write(&archive_path, bytes)
        .await
        .with_context(|| format!("write runner archive {}", archive_path.display()))?;

    Ok(archive_path)
}

async fn extract_runner_archive(archive_path: &PathBuf, job_dir: &PathBuf) -> Result<()> {
    let status = Command::new("tar")
        .arg("-xzf")
        .arg(archive_path)
        .arg("-C")
        .arg(job_dir)
        .status()
        .await
        .context("extract github runner archive")?;

    if !status.success() {
        let code = status.code().unwrap_or(-1);
        bail!("tar exited with {code}");
    }

    Ok(())
}

async fn configure_runner(
    job_dir: &PathBuf,
    runner_registration_url: &str,
    runner_registration_token: &str,
    runner_name: &str,
    labels: &[String],
) -> Result<()> {
    let status = Command::new("./config.sh")
        .current_dir(job_dir)
        .arg("--url")
        .arg(runner_registration_url)
        .arg("--token")
        .arg(runner_registration_token)
        .arg("--name")
        .arg(runner_name)
        .arg("--labels")
        .arg(labels.join(","))
        .arg("--work")
        .arg("_work")
        .arg("--ephemeral")
        .arg("--unattended")
        .arg("--replace")
        .status()
        .await
        .context("configure github runner")?;

    if !status.success() {
        let code = status.code().unwrap_or(-1);
        bail!("config.sh exited with {code}");
    }

    Ok(())
}

fn runner_artifact_name() -> Result<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "x86_64") => Ok("actions-runner-osx-x64-2.328.0.tar.gz"),
        ("macos", "aarch64") => Ok("actions-runner-osx-arm64-2.328.0.tar.gz"),
        ("linux", "x86_64") => Ok("actions-runner-linux-x64-2.328.0.tar.gz"),
        ("linux", "aarch64") => Ok("actions-runner-linux-arm64-2.328.0.tar.gz"),
        (os, arch) => Err(anyhow!("unsupported host for github runner: {os}/{arch}")),
    }
}

fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .unwrap_or_else(|_| "unknown".into())
        .trim()
        .to_string()
}
