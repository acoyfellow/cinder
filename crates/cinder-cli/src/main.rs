use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Read;
use std::path::Path;
use std::process::{exit, Command, Stdio};

#[derive(Parser)]
#[command(name = "cinder", about = "open source CI runner acceleration on cloudflare")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// provision cinder infra and verify it works
    Deploy {
        #[arg(long)]
        account_id: Option<String>,

        #[arg(long)]
        api_token: Option<String>,

        #[arg(long)]
        state_bucket: Option<String>,

        #[arg(long)]
        region: Option<StateRegion>,
    },
    /// start a runner agent on this machine
    Agent {
        #[command(subcommand)]
        cmd: AgentCommands,
    },
    /// manage auth tokens
    Token {
        #[command(subcommand)]
        cmd: TokenCommands,
    },
    /// manage connected repos
    Repo {
        #[command(subcommand)]
        cmd: RepoCommands,
    },
}

#[derive(Subcommand)]
enum AgentCommands {
    /// start polling for jobs
    Start {
        #[arg(long)]
        url: Option<String>,

        #[arg(long)]
        token: Option<String>,

        #[arg(long)]
        labels: Option<String>,

        #[arg(long)]
        poll_ms: Option<u64>,

        #[arg(long)]
        cache_dir: Option<String>,
    },
}

#[derive(Subcommand)]
enum TokenCommands {
    /// rotate the internal auth token
    Rotate,
}

#[derive(Subcommand)]
enum RepoCommands {
    /// connect a GitHub repo to this cinder deployment
    Connect {
        repo: String,

        #[arg(long, default_value = "main")]
        branch: String,

        #[arg(long, default_value = ".github/workflows/ci.yml")]
        workflow: String,
    },
}

#[derive(Clone, Debug, ValueEnum)]
enum StateRegion {
    Auto,
    Wnam,
    Enam,
    Weur,
    Eeur,
    Apac,
}

impl StateRegion {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Wnam => "wnam",
            Self::Enam => "enam",
            Self::Weur => "weur",
            Self::Eeur => "eeur",
            Self::Apac => "apac",
        }
    }
}

#[derive(Debug, Deserialize)]
struct RuntimeManifest {
    #[serde(rename = "orchestratorUrl")]
    orchestrator_url: String,
}

#[derive(Debug, Serialize)]
struct ConnectRepoRequest {
    repo: String,
    branch: String,
    workflow: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let repo_root = std::env::current_dir().context("failed to resolve current working directory")?;

    match cli.command {
        Commands::Deploy {
            account_id,
            api_token,
            state_bucket,
            region,
        } => {
            if region.is_some() && state_bucket.is_none() {
                return Err(anyhow::anyhow!("--region requires --state-bucket"));
            }

            let mut extra_env = Vec::new();

            if let Some(account_id) = account_id {
                extra_env.push(("CLOUDFLARE_ACCOUNT_ID".to_string(), account_id));
            }

            if let Some(api_token) = api_token {
                extra_env.push(("CLOUDFLARE_API_TOKEN".to_string(), api_token));
            }

            if let Some(state_bucket) = state_bucket {
                extra_env.push(("CINDER_STATE_BUCKET".to_string(), state_bucket));
                extra_env.push((
                    "CINDER_STATE_REGION".to_string(),
                    region.unwrap_or(StateRegion::Auto).as_str().to_string(),
                ));
            }

            run_command(
                &repo_root,
                "bun",
                ["run", "provision"],
                (!extra_env.is_empty()).then_some(extra_env.as_slice()),
            )?;
        }
        Commands::Agent {
            cmd:
                AgentCommands::Start {
                    url,
                    token,
                    labels,
                    poll_ms,
                    cache_dir,
                },
        } => {
            let url = match url {
                Some(url) => url,
                None => resolve_base_url(&repo_root)?,
            };
            let token = match token {
                Some(token) => token,
                None => resolve_agent_token()?,
            };
            let mut args = vec![
                "run".to_string(),
                "--quiet".to_string(),
                "-p".to_string(),
                "cinder-agent".to_string(),
                "--".to_string(),
                "--url".to_string(),
                url,
                "--token".to_string(),
                token,
            ];

            if let Some(labels) = labels.or_else(|| optional_env("CINDER_LABELS")) {
                args.push("--labels".to_string());
                args.push(labels);
            }

            if let Some(poll_ms) = poll_ms {
                args.push("--poll-ms".to_string());
                args.push(poll_ms.to_string());
            }

            if let Some(cache_dir) = cache_dir {
                args.push("--cache-dir".to_string());
                args.push(cache_dir);
            }

            run_command(
                &repo_root,
                "cargo",
                args.iter().map(String::as_str),
                None,
            )?;
        }
        Commands::Token { cmd: TokenCommands::Rotate } => {
            let token = generate_token()?;
            write_env_var(&repo_root.join(".env"), "CINDER_INTERNAL_TOKEN", &token)?;
            run_command(
                &repo_root,
                "bun",
                ["run", "provision"],
                None,
            )?;
            println!("{token}");
        }
        Commands::Repo {
            cmd:
                RepoCommands::Connect {
                    repo,
                    branch,
                    workflow,
                },
        } => {
            let base_url = resolve_base_url(&repo_root)?;
            let token = resolve_agent_token()?;
            let client = reqwest::Client::new();
            let response = client
                .post(format!("{}/repos/connect", base_url.trim_end_matches('/')))
                .bearer_auth(token)
                .json(&ConnectRepoRequest {
                    repo: repo.clone(),
                    branch,
                    workflow,
                })
                .send()
                .await
                .context("failed to connect repo")?;

            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| String::from("failed to read response body"));
            if !status.is_success() {
                return Err(anyhow::anyhow!(
                    "repo connect failed with {}: {}",
                    status,
                    body
                ));
            }

            println!("connected {repo}");
        }
    }

    Ok(())
}

fn run_command<'a, I>(
    cwd: &Path,
    program: &str,
    args: I,
    extra_env: Option<&[(String, String)]>,
) -> Result<()>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut command = Command::new(program);
    command
        .current_dir(cwd)
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    if let Some(extra_env) = extra_env {
        command.envs(extra_env.iter().map(|(key, value)| (key, value)));
    }

    let status = command
        .status()
        .with_context(|| format!("failed to start {program}"))?;

    if status.success() {
        return Ok(());
    }

    exit(status.code().unwrap_or(1));
}

fn optional_env(name: &str) -> Option<String> {
    std::env::var(name).ok().and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn resolve_agent_token() -> Result<String> {
    optional_env("CINDER_TOKEN")
        .or_else(|| optional_env("CINDER_INTERNAL_TOKEN"))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "missing agent token; set --token, CINDER_TOKEN, or CINDER_INTERNAL_TOKEN"
            )
        })
}

fn resolve_base_url(repo_root: &Path) -> Result<String> {
    if let Some(url) = optional_env("CINDER_URL") {
        return Ok(url);
    }

    if let Some(url) = optional_env("CINDER_BASE_URL") {
        return Ok(url);
    }

    let runtime_path = repo_root.join(".gateproof/runtime.json");

    let runtime_contents = fs::read_to_string(&runtime_path)
        .with_context(|| format!("failed to read {}", runtime_path.display()))?;
    let manifest: RuntimeManifest = serde_json::from_str(&runtime_contents)
        .with_context(|| format!("failed to parse {}", runtime_path.display()))?;

    Ok(manifest.orchestrator_url)
}

fn generate_token() -> Result<String> {
    let mut bytes = [0_u8; 32];
    let mut file = fs::File::open("/dev/urandom").context("failed to open /dev/urandom")?;
    file.read_exact(&mut bytes)
        .context("failed to read random bytes for token rotation")?;
    Ok(hex::encode(bytes))
}

fn write_env_var(path: &Path, key: &str, value: &str) -> Result<()> {
    let mut contents = if path.exists() {
        fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?
    } else {
        String::new()
    };

    let mut found = false;
    let mut next_lines = Vec::new();

    for line in contents.lines() {
        if line.starts_with(&format!("{key}=")) {
            next_lines.push(format!("{key}={value}"));
            found = true;
        } else {
            next_lines.push(line.to_string());
        }
    }

    if !found {
        if !next_lines.is_empty() && !next_lines.last().is_some_and(String::is_empty) {
            next_lines.push(String::new());
        }
        next_lines.push(format!("{key}={value}"));
    }

    contents = if next_lines.is_empty() {
        format!("{key}={value}\n")
    } else {
        format!("{}\n", next_lines.join("\n"))
    };

    fs::write(path, contents).with_context(|| format!("failed to write {}", path.display()))?;

    Ok(())
}
