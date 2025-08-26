use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use clap::{ValueEnum};
use tokio::time::{sleep, Duration};
use std::{fs, path::Path};
use serde::{Serialize, Deserialize};

mod starthub_api;
mod ghapp;
mod config; // 👈 add
mod runners;

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
enum RunnerKind {
    Github,
    Local, // placeholder for future
}

// ---- Starthub manifest schema ----
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ShManifest {
    name: String,
    version: String,
    kind: ShKind,                 // 👈 new
    ref_field: String, // serialize as "ref"
    inputs: Vec<ShPort>,
    outputs: Vec<ShPort>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum ShKind { Wasm, Docker }      // 👈 new

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ShPort {
    name: String,
    description: String,
    #[serde(rename = "type")]
    ty: ShType,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum ShType {
    String,
    Integer,
    Boolean,
    Object,
    Array,
    Number,
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
    Run {
        /// Package slug/name, e.g. "chirpstack"
        action: String,       
        /// Repeatable env secret: -e KEY=VALUE (will become a repo secret)
        #[arg(short = 'e', long = "env", value_name = "KEY=VALUE")]
        secrets: Vec<String>,                    // <— collect multiple -e
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
        Commands::Run { action, secrets, env, runner } => cmd_run(action, secrets, env, runner).await?,
        Commands::Status { id } => cmd_status(id).await?,
    }
    Ok(())
}

async fn cmd_init(path: String) -> Result<()> {
    use inquire::{Text, Select};

    // Basic fields
    let name = Text::new("Package name:")
        .with_default("http-get-wasm")
        .prompt()?;

    let version = Text::new("Version:")
        .with_default("0.0.1")
        .prompt()?;

    // Kind
    let kind_str = Select::new("Kind:", vec!["wasm", "docker"]).prompt()?;
    let kind = match kind_str {
        "wasm" => ShKind::Wasm,
        "docker" => ShKind::Docker,
        _ => unreachable!(),
    };

    // A single reference works for both: wasm (module URL/OCI) or docker (image ref)
    let ref_default = match kind {
        ShKind::Wasm => "https://github.com/starthubhq/http-get-wasm/releases/download/v0.0.1/module.wasm",
        ShKind::Docker => "ghcr.io/owner/image:tag",
    };
    let ref_field = Text::new("Module/Image reference (URL or OCI):")
        .with_help_message("e.g. WASM URL or ghcr.io/org/image:tag")
        .with_default(ref_default)
        .prompt()?
        .to_string();

    // Empty vectors for now (you can add your inputs/outputs loop later)
    let inputs: Vec<ShPort> = Vec::new();
    let outputs: Vec<ShPort> = Vec::new();

    // Build manifest and write file
    let m = ShManifest { name, version, kind, ref_field, inputs, outputs };
    let json = serde_json::to_string_pretty(&m)?;

    let out_dir = std::path::Path::new(&path);
    if !out_dir.exists() {
        std::fs::create_dir_all(out_dir)?;
    }
    let out_file = out_dir.join("starthub.json");
    std::fs::write(&out_file, json)?;

    println!("✓ Wrote {}", out_file.display());
    Ok(())
}


async fn cmd_login(runner: RunnerKind) -> anyhow::Result<()> {
    let r = make_runner(runner);
    println!("→ Logging in for runner: {}", r.name());
    r.ensure_auth().await?;
    println!("✓ Login complete for {}", r.name());
    Ok(())
}

// Parse KEY=VALUE items into Vec<(String,String)>, with friendly errors.
fn parse_secret_pairs(items: &[String]) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    for raw in items {
        let (k, v) = raw
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!(format!("invalid -e value '{raw}', expected KEY=VALUE")))?;
        if k.trim().is_empty() {
            anyhow::bail!("secret name empty in '{raw}'");
        }
        out.push((k.trim().to_string(), v.to_string()));
    }
    Ok(out)
}

fn open_actions_page(owner: &str, repo: &str) {
    let url = format!("https://github.com/{owner}/{repo}/actions");
    match webbrowser::open(&url) {
        Ok(_) => println!("↗ Opened GitHub Actions: {url}"),
        Err(e) => println!("→ GitHub Actions: {url} (couldn't auto-open: {e})"),
    }
}

async fn cmd_run(action: String, secrets: Vec<String>, env: Option<String>, runner: RunnerKind) -> Result<()> {
    let parsed_secrets = parse_secret_pairs(&secrets)?;
    let mut ctx = runners::DeployCtx {
        action,
        env,
        owner: None,
        repo: None,
        secrets: parsed_secrets,       // <— pass to runner
    };
    let r = make_runner(runner);

    // 1) ensure auth for selected runner; guide if missing
    r.ensure_auth().await?;

    // 2) do the runner-specific steps
    r.prepare(&mut ctx).await?;
    r.put_files(&ctx).await?;
    r.set_secrets(&ctx).await?;       // <— will create repo secrets
    r.dispatch(&ctx).await?;

    if let (Some(owner), Some(repo)) = (ctx.owner.as_deref(), ctx.repo.as_deref()) {
        sleep(Duration::from_secs(5)).await;
        open_actions_page(owner, repo);
    }

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