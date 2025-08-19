use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

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
    /// Login (device code / browser)
    Login,
    /// Deploy with the given config
    Deploy {
        /// Path to starthub.yaml/json
        #[arg(long, default_value = "starthub.yaml")]
        config: String,
        /// Optional environment name
        #[arg(long)]
        env: Option<String>,
    },
    /// Show deployment status
    Status {
        #[arg(long)]
        id: Option<String>,
    },
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
        Commands::Init { path } => cmd_init(path).await?,
        Commands::Login => cmd_login().await?,
        Commands::Deploy { config, env } => cmd_deploy(config, env).await?,
        Commands::Status { id } => cmd_status(id).await?,
    }
    Ok(())
}

async fn cmd_init(path: String) -> Result<()> {
    println!("Init in {path}");
    // TODO: write default config, detect repo, etc.
    Ok(())
}

async fn cmd_login() -> Result<()> {
    println!("Login flow…");
    // TODO: device-code / browser login; store token in OS keychain or config dir
    Ok(())
}

async fn cmd_deploy(config: String, env: Option<String>) -> Result<()> {
    println!("Deploying with {config} (env={env:?})");
    // TODO: call Starthub API
    Ok(())
}

async fn cmd_status(id: Option<String>) -> Result<()> {
    println!("Status for {id:?}");
    // TODO: poll API
    Ok(())
}
