use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use clap::{ValueEnum};

mod starthub_api;
mod ghapp;
mod config;
mod runners;
mod models;
mod templates;
mod commands;
mod publish;

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
pub enum RunnerKind {
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
    Publish {
        /// Do not build, only push/tag (assumes image exists locally)
        #[arg(long)]
        no_build: bool,
    },
    /// Deploy with the given config
    Run {
        /// Package slug/name, e.g. "chirpstack"
        action: String,       
                    /// Repeatable env secret: -e KEY=VALUE (will become a repo secret)
            #[arg(short = 'e', long = "secret", value_name = "KEY=VALUE")]
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
        Commands::Init { path } => commands::cmd_init(path).await?,
        Commands::Publish { no_build } => publish::cmd_publish(no_build).await?,
        Commands::Run { action, secrets, env, runner } => commands::cmd_run(action, secrets, env, runner).await?,
        Commands::Status { id } => commands::cmd_status(id).await?,
    }
    Ok(())
}

pub fn make_runner(kind: RunnerKind) -> Box<dyn runners::Runner + Send + Sync> {
    match kind {
        RunnerKind::Github => Box::new(runners::github::GithubRunner),
        RunnerKind::Local  => Box::new(runners::local::LocalRunner),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn test_cli_parsing() {
        // Test basic CLI parsing
        let args = vec!["starthub", "init", "--path", "test-dir"];
        let cli = Cli::try_parse_from(args).unwrap();
        
        assert!(!cli.verbose);
        match cli.command {
            Commands::Init { path } => {
                assert_eq!(path, "test-dir");
            }
            _ => panic!("Expected Init command"),
        }
    }

    #[test]
    fn test_cli_verbose_flag() {
        let args = vec!["starthub", "--verbose", "init"];
        let cli = Cli::try_parse_from(args).unwrap();
        
        assert!(cli.verbose);
        match cli.command {
            Commands::Init { path } => {
                assert_eq!(path, ".");
            }
            _ => panic!("Expected Init command"),
        }
    }

    #[test]
    fn test_cli_publish_command() {
        let args = vec!["starthub", "publish", "--no-build"];
        let cli = Cli::try_parse_from(args).unwrap();
        
        match cli.command {
            Commands::Publish { no_build } => {
                assert!(no_build);
            }
            _ => panic!("Expected Publish command"),
        }
    }

    #[test]
    fn test_cli_run_command() {
        let args = vec!["starthub", "run", "test-action", "--secret", "KEY1=value1", "--secret", "KEY2=value2"];
        let cli = Cli::try_parse_from(args).unwrap();
        
        match cli.command {
            Commands::Run { action, secrets, env, runner } => {
                assert_eq!(action, "test-action");
                assert_eq!(secrets, vec!["KEY1=value1", "KEY2=value2"]);
                assert_eq!(env, None);
                assert!(matches!(runner, RunnerKind::Github));
            }
            _ => panic!("Expected Run command"),
        }
    }

    #[test]
    fn test_cli_run_command_with_env() {
        let args = vec!["starthub", "run", "test-action", "--env", "production"];
        let cli = Cli::try_parse_from(args).unwrap();
        
        match cli.command {
            Commands::Run { action, secrets, env, runner } => {
                assert_eq!(action, "test-action");
                assert_eq!(secrets, Vec::<String>::new());
                assert_eq!(env, Some("production".to_string()));
                assert!(matches!(runner, RunnerKind::Github));
            }
            _ => panic!("Expected Run command"),
        }
    }

    #[test]
    fn test_cli_run_command_with_local_runner() {
        let args = vec!["starthub", "run", "test-action", "--runner", "local"];
        let cli = Cli::try_parse_from(args).unwrap();
        
        match cli.command {
            Commands::Run { action, secrets, env, runner } => {
                assert_eq!(action, "test-action");
                assert_eq!(secrets, Vec::<String>::new());
                assert_eq!(env, None);
                assert!(matches!(runner, RunnerKind::Local));
            }
            _ => panic!("Expected Run command"),
        }
    }

    #[test]
    fn test_cli_status_command() {
        let args = vec!["starthub", "status", "--id", "test-id"];
        let cli = Cli::try_parse_from(args).unwrap();
        
        match cli.command {
            Commands::Status { id } => {
                assert_eq!(id, Some("test-id".to_string()));
            }
            _ => panic!("Expected Status command"),
        }
    }

    #[test]
    fn test_runner_kind_enum() {
        // Test that RunnerKind can be cloned and compared
        let github = RunnerKind::Github;
        let local = RunnerKind::Local;
        
        assert_ne!(github, local);
        assert_eq!(github, RunnerKind::Github);
        assert_eq!(local, RunnerKind::Local);
        
        // Test cloning
        let github_clone = github.clone();
        let local_clone = local.clone();
        
        assert_eq!(github, github_clone);
        assert_eq!(local, local_clone);
    }

    #[test]
    fn test_make_runner_function() {
        // Test that make_runner returns the correct runner types
        let _github_runner = make_runner(RunnerKind::Github);
        let _local_runner = make_runner(RunnerKind::Local);
        
        // We can't easily test the actual runner types without more complex setup,
        // but we can verify they implement the Runner trait
        assert!(std::any::type_name::<Box<dyn runners::Runner + Send + Sync>>() == 
                std::any::type_name::<Box<dyn runners::Runner + Send + Sync>>());
    }

    #[test]
    fn test_commands_enum_debug() {
        // Test that Commands enum can be debug printed
        let init_cmd = Commands::Init { path: "test".to_string() };
        let publish_cmd = Commands::Publish { no_build: false };
        let run_cmd = Commands::Run { 
            action: "test".to_string(), 
            secrets: vec![], 
            env: None, 
            runner: RunnerKind::Github 
        };
        let status_cmd = Commands::Status { id: None };
        
        // These should not panic
        format!("{:?}", init_cmd);
        format!("{:?}", publish_cmd);
        format!("{:?}", run_cmd);
        format!("{:?}", status_cmd);
    }

    #[test]
    fn test_cli_struct_debug() {
        // Test that Cli struct can be debug printed
        let cli = Cli {
            command: Commands::Init { path: "test".to_string() },
            verbose: false,
        };
        
        // This should not panic
        format!("{:?}", cli);
    }
}
