use anyhow::{anyhow, bail, Context, Result};
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::{fs, process::Command};
use tracing::{error, info, warn};

const RUNNER_VERSION: &str = "2.332.0";

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

#[derive(Debug, Deserialize)]
struct CacheRestoreResponse {
    miss: Option<bool>,
    url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CacheUploadResponse {
    url: Option<String>,
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

        let run_id_label = run_id
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let repo_label = repo_full_name
            .as_deref()
            .unwrap_or("unknown");

        println!(
            "accepted job {} for run {} repo {}",
            job_id, run_id_label, repo_label
        );
        info!(
            "accepted job {} for run {} repo {}",
            job_id, run_id_label, repo_label
        );
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
    let cache_root = args.cache_dir.join("build-cache");
    let cargo_home = cache_root.join("cargo-home");
    let cargo_target = cache_root.join("cargo-target");
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
    reset_cache_dirs(&cache_root, &cargo_home, &cargo_target).await?;

    if let Some(cache_key) = job.cache_key.as_deref() {
        restore_cache(
            client,
            &args.url,
            &args.token,
            job_id,
            cache_key,
            &cache_root,
            &args.cache_dir,
        )
        .await?;
    }

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

    let output = Command::new("./run.sh")
        .current_dir(&job_dir)
        .env("CARGO_HOME", &cargo_home)
        .env("CARGO_TARGET_DIR", &cargo_target)
        .output()
        .await
        .context("start github runner")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stdout.is_empty() {
        print!("{stdout}");
    }
    if !stderr.is_empty() {
        eprint!("{stderr}");
    }

    let exit_code = output.status.code().unwrap_or(-1);
    let combined_output = format!("{stdout}\n{stderr}");
    let workflow_result = if combined_output.contains("completed with result: Succeeded") {
        Some("Succeeded")
    } else if combined_output.contains("completed with result: Failed") {
        Some("Failed")
    } else if combined_output.contains("completed with result: Cancelled") {
        Some("Cancelled")
    } else if combined_output.contains("completed with result: Canceled") {
        Some("Canceled")
    } else {
        None
    };

    println!("job {} completed with exit code {}", job_id, exit_code);
    info!("job {} completed with exit code {}", job_id, exit_code);

    if output.status.success() && workflow_result == Some("Succeeded") {
        if let Some(cache_key) = job.cache_key.as_deref() {
            upload_cache(
                client,
                &args.url,
                &args.token,
                job_id,
                cache_key,
                &cache_root,
                &args.cache_dir,
            )
            .await?;
        }

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
    match workflow_result {
        Some(result) => bail!("github job finished with {result} (runner exit code {exit_code})"),
        None => bail!("github runner exited with {exit_code} without a terminal job result"),
    }
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

async fn reset_cache_dirs(cache_root: &PathBuf, cargo_home: &PathBuf, cargo_target: &PathBuf) -> Result<()> {
    if fs::try_exists(cargo_home).await.unwrap_or(false) {
        fs::remove_dir_all(cargo_home)
            .await
            .with_context(|| format!("remove stale cargo home {}", cargo_home.display()))?;
    }

    if fs::try_exists(cargo_target).await.unwrap_or(false) {
        fs::remove_dir_all(cargo_target)
            .await
            .with_context(|| format!("remove stale cargo target {}", cargo_target.display()))?;
    }

    fs::create_dir_all(cache_root)
        .await
        .with_context(|| format!("create cache root {}", cache_root.display()))?;
    fs::create_dir_all(cargo_home)
        .await
        .with_context(|| format!("create cargo home {}", cargo_home.display()))?;
    fs::create_dir_all(cargo_target)
        .await
        .with_context(|| format!("create cargo target {}", cargo_target.display()))?;

    Ok(())
}

async fn restore_cache(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
    job_id: u64,
    cache_key: &str,
    cache_root: &PathBuf,
    scratch_dir: &PathBuf,
) -> Result<()> {
    let response = client
        .post(format!("{}/cache/restore/{}", base_url, cache_key))
        .bearer_auth(token)
        .send()
        .await
        .context("request cache restore")?
        .error_for_status()
        .context("cache restore request failed")?;

    let restore: CacheRestoreResponse = response.json().await.context("decode cache restore response")?;

    if restore.miss.unwrap_or(false) || restore.url.as_deref().unwrap_or("").is_empty() {
        println!("cache miss for job {}", job_id);
        info!("cache miss for job {}", job_id);
        return Ok(());
    }

    let archive_path = scratch_dir.join(format!("cache-restore-{job_id}.tar.xz"));
    let archive_bytes = client
        .get(
            restore
                .url
                .as_deref()
                .context("cache restore response missing url")?,
        )
        .send()
        .await
        .context("download cache archive")?
        .error_for_status()
        .context("cache archive download failed")?
        .bytes()
        .await
        .context("read cache archive bytes")?;

    fs::write(&archive_path, archive_bytes)
        .await
        .with_context(|| format!("write cache restore archive {}", archive_path.display()))?;

    let status = Command::new("tar")
        .arg("-xJf")
        .arg(&archive_path)
        .arg("-C")
        .arg(cache_root)
        .status()
        .await
        .context("extract cache archive")?;

    let _ = fs::remove_file(&archive_path).await;

    if !status.success() {
        let code = status.code().unwrap_or(-1);
        bail!("cache restore extraction failed with {code}");
    }

    println!("cache restored for job {}", job_id);
    info!("cache restored for job {}", job_id);
    Ok(())
}

async fn upload_cache(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
    job_id: u64,
    cache_key: &str,
    cache_root: &PathBuf,
    scratch_dir: &PathBuf,
) -> Result<()> {
    let archive_path = scratch_dir.join(format!("cache-upload-{job_id}.tar.xz"));
    let mut archive_entries = Vec::new();

    for relative in [
        "cargo-home/registry/cache",
        "cargo-home/registry/index",
        "cargo-target/release/.fingerprint",
    ] {
        if fs::try_exists(cache_root.join(relative)).await.unwrap_or(false) {
            archive_entries.push(relative.to_string());
        }
    }

    if archive_entries.is_empty() {
        warn!("no cacheable artifacts found for job {}", job_id);
        return Ok(());
    }

    let status = Command::new("tar")
        .current_dir(cache_root)
        .arg("-cJf")
        .arg(&archive_path)
        .args(&archive_entries)
        .status()
        .await
        .context("create cache archive")?;

    if !status.success() {
        let code = status.code().unwrap_or(-1);
        bail!("cache archive creation failed with {code}");
    }

    let size_bytes = fs::metadata(&archive_path)
        .await
        .with_context(|| format!("stat cache archive {}", archive_path.display()))?
        .len();

    let upload_response = client
        .post(format!("{}/cache/upload", base_url))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "key": cache_key,
            "content_type": "application/x-xz",
            "size_bytes": size_bytes,
        }))
        .send()
        .await
        .context("request cache upload url")?
        .error_for_status()
        .context("cache upload url request failed")?;

    let upload: CacheUploadResponse = upload_response
        .json()
        .await
        .context("decode cache upload response")?;
    let upload_url = upload.url.as_deref().context("cache upload response missing url")?;
    let archive_bytes = fs::read(&archive_path)
        .await
        .with_context(|| format!("read cache archive {}", archive_path.display()))?;

    client
        .put(upload_url)
        .body(archive_bytes)
        .send()
        .await
        .context("upload cache archive")?
        .error_for_status()
        .context("cache archive upload failed")?;

    let _ = fs::remove_file(&archive_path).await;

    println!("cache uploaded for job {}", job_id);
    info!("cache uploaded for job {}", job_id);
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
        ("macos", "x86_64") => Ok("actions-runner-osx-x64-2.332.0.tar.gz"),
        ("macos", "aarch64") => Ok("actions-runner-osx-arm64-2.332.0.tar.gz"),
        ("linux", "x86_64") => Ok("actions-runner-linux-x64-2.332.0.tar.gz"),
        ("linux", "aarch64") => Ok("actions-runner-linux-arm64-2.332.0.tar.gz"),
        (os, arch) => Err(anyhow!("unsupported host for github runner: {os}/{arch}")),
    }
}

fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .unwrap_or_else(|_| "unknown".into())
        .trim()
        .to_string()
}
