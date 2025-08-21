use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use clap::{ValueEnum};

mod ghapp;
mod config; // 👈 add
mod runners;

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
enum RunnerKind {
    Github,
    Local, // placeholder for future
}

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
    /// Deploy with the given config
    Deploy {
        /// Package slug/name, e.g. "chirpstack"
        action: String,       
        /// Choose where to run the deployment
        #[arg(long, value_enum, default_value_t = RunnerKind::Github)]
        runner: RunnerKind,
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
        Commands::Deploy { action, env, runner } => cmd_deploy(action, env, runner).await?,
        Commands::Status { id } => cmd_status(id).await?,
    }
    Ok(())
}

async fn cmd_init(path: String) -> Result<()> {
    println!("Init in {path}");
    // TODO: write default config, detect repo, etc.
    Ok(())
}

async fn cmd_login(runner: RunnerKind) -> anyhow::Result<()> {
    let r = make_runner(runner);
    println!("→ Logging in for runner: {}", r.name());
    r.ensure_auth().await?;
    println!("✓ Login complete for {}", r.name());
    Ok(())
}

async fn cmd_deploy(action: String, env: Option<String>, runner: RunnerKind) -> Result<()> {
     let mut ctx = runners::DeployCtx {
        action,
        env,
        owner: None,
        repo: None,
    };
    let r = make_runner(runner);

    // 1) ensure auth for selected runner; guide if missing
    r.ensure_auth().await?;

    // 2) do the runner-specific steps
    r.prepare(&mut ctx).await?;
    r.put_files(&ctx).await?;
    r.set_secrets(&ctx).await?;
    r.dispatch(&ctx).await?;

    println!("✓ Dispatch complete for {}", r.name());
    Ok(())
}

async fn cmd_status(id: Option<String>) -> Result<()> {
    println!("Status for {id:?}");
    // TODO: poll API
    Ok(())
}

pub fn make_runner(kind: RunnerKind) -> Box<dyn runners::Runner + Send + Sync> {
    match kind {
        RunnerKind::Github => Box::new(runners::github::GithubRunner),
        RunnerKind::Local  => Box::new(runners::local::LocalRunner),
    }
}