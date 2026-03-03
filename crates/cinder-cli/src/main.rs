use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde::Deserialize;
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
    Deploy,
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
}

#[derive(Subcommand)]
enum AgentCommands {
    /// start polling for jobs
    Start,
}

#[derive(Subcommand)]
enum TokenCommands {
    /// rotate the internal auth token
    Rotate,
}

#[derive(Debug, Deserialize)]
struct RuntimeManifest {
    #[serde(rename = "orchestratorUrl")]
    orchestrator_url: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let repo_root = std::env::current_dir().context("failed to resolve current working directory")?;

    match cli.command {
        Commands::Deploy => {
            run_command(
                &repo_root,
                "bun",
                ["run", "provision"],
                None,
            )?;
        }
        Commands::Agent { cmd: AgentCommands::Start } => {
            let url = resolve_base_url(&repo_root)?;
            let token = require_env("CINDER_INTERNAL_TOKEN")?;
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

            if let Some(labels) = optional_env("CINDER_LABELS") {
                args.push("--labels".to_string());
                args.push(labels);
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
    }

    Ok(())
}

fn run_command<'a, I>(
    cwd: &Path,
    program: &str,
    args: I,
    extra_env: Option<&[(&str, &str)]>,
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
        command.envs(extra_env.iter().copied());
    }

    let status = command
        .status()
        .with_context(|| format!("failed to start {program}"))?;

    if status.success() {
        return Ok(());
    }

    exit(status.code().unwrap_or(1));
}

fn require_env(name: &str) -> Result<String> {
    optional_env(name).ok_or_else(|| anyhow::anyhow!("missing required environment variable {name}"))
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

fn resolve_base_url(repo_root: &Path) -> Result<String> {
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
