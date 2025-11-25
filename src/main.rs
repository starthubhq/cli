use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod starthub_api;
mod ghapp;
mod config;
mod models;
mod templates;
mod commands;
mod publish;


#[derive(Parser, Debug)]
#[command(name="starthub", version, about="Starthub CLI")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
    /// Verbose logs
    #[arg(short, long, global = true)]
    verbose: bool,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Initialize a project (creates config, etc.)
    Init {
        #[arg(long, default_value = ".")]
        path: String,
    },
    Publish {
        /// Do not build, only push/tag (assumes image exists locally)
        #[arg(long)]
        no_build: bool,
    },
    /// Deploy with the given config
    Run {
        /// Package slug/name, e.g. "chirpstack"
        action: String,       
    },
    /// Start the server in detached mode
    Start {
        /// Host to bind to
        #[arg(long, default_value = "127.0.0.1:3000")]
        bind: String,
    },
    /// Stop the running server
    Stop,
    /// Show server logs
    Logs {
        /// Follow log output (like tail -f)
        #[arg(short, long)]
        follow: bool,
        /// Number of lines to show from the end
        #[arg(short, long, default_value = "100")]
        lines: usize,
    },
    /// Show server status
    Status,
    /// Authenticate with Starthub backend
    Login {
        /// Starthub API base URL
        #[arg(long, default_value = "https://api.starthub.so")]
        api_base: String,
    },
    /// Logout from Starthub backend
    Logout,
    Auth,
    /// Clear the cache
    Reset,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let filter = if cli.verbose { "info" } else { "warn" };
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("STARTHUB_LOG").unwrap_or_else(|_| filter.into()),
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();

    match cli.command {
        Commands::Init { path } => commands::cmd_init(path).await?,
        Commands::Publish { no_build } => publish::cmd_publish(no_build).await?,
        Commands::Run { action } => commands::cmd_run(action).await?,
        Commands::Start { bind } => commands::cmd_start(bind).await?,
        Commands::Stop => commands::cmd_stop().await?,
        Commands::Logs { follow, lines } => commands::cmd_logs(follow, lines).await?,
        Commands::Status => commands::cmd_status().await?,
        Commands::Login { api_base } => commands::cmd_login_starthub(api_base).await?,
        Commands::Logout => commands::cmd_logout_starthub().await?,
        Commands::Auth => commands::cmd_auth_status().await?,
        Commands::Reset => commands::cmd_reset().await?,
    }
    Ok(())
}