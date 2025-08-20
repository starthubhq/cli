use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use std::time::Duration;

mod ghapp;
mod config; // 👈 add

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

async fn cmd_login() -> anyhow::Result<()> {
    let client_id = config::GH_CLIENT_ID;
    let app_id    = config::GH_APP_ID;
    let app_slug  = config::GH_APP_SLUG;

    // 1) Device flow → UAT
    let token = ghapp::device_login(client_id).await?;
    let me = ghapp::get_user(&token.access_token).await?;
    println!("✓ Authorized as {}", me.login);

    // 2) Save token (so we keep it even if the install step takes time)
    ghapp::save_token(&token)?;

    // 3) Ensure the app is installed somewhere for this user
    match ghapp::find_installation_for_app(&token.access_token, app_id).await? {
        Some(inst) => {
            println!(
                "✓ App already installed for {} ({}) [installation {}]",
                inst.account.login, inst.account.account_type, inst.id
            );
        }
        None => {
            let install_url = format!("https://github.com/apps/{}/installations/new", app_slug);
            println!("→ App not installed yet. Opening install page…\n{install_url}\n");
            let _ = webbrowser::open(&install_url); // ignore errors; user can copy the URL
            ghapp::wait_for_installation(&token.access_token, app_id, Duration::from_secs(300)).await?;
        }
    }

    println!("✓ Login complete.");
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
