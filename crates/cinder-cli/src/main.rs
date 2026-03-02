use clap::{Parser, Subcommand};

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

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Deploy => {
            println!("run: bun prd.ts");
        }
        Commands::Agent { cmd: AgentCommands::Start } => {
            println!("run: cinder-agent (see crates/cinder-agent)");
        }
        Commands::Token { cmd: TokenCommands::Rotate } => {
            println!("not implemented yet");
        }
    }
}
