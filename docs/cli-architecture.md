# StartHub CLI Architecture

This document describes the architecture of the StartHub CLI, which uses a hybrid approach combining a Rust CLI with a Rust local server for optimal performance and reliability.

## Overview

The StartHub CLI follows a two-tier architecture:

1. **CLI Layer (Rust)** - Handles user interaction, command parsing, and orchestration
2. **Server Layer (Rust)** - Manages Docker containers, WASM execution, and heavy computational tasks

## Architecture Pattern

```
┌─────────────────────────────────────┐
│           User Interface             │
│         (CLI Commands)              │
└─────────────────────────────────────┘
                    │
                    ▼
┌─────────────────────────────────────┐
│         CLI Layer (Rust)        │
│  • Command parsing                  │
│  • User interaction                 │
│  • Server orchestration            │
│  • Platform detection              │
└─────────────────────────────────────┘
                    │
                    ▼
┌─────────────────────────────────────┐
│       Server Layer (Rust)           │
│  • Docker container management     │
│  • WASM execution                   │
│  • File watching                   │
│  • HTTP API                        │
└─────────────────────────────────────┘
                    │
                    ▼
┌─────────────────────────────────────┐
│        Execution Environment        │
│  • Docker containers               │
│  • WASM modules                    │
│  • File system                     │
└─────────────────────────────────────┘
```

## Package Structure

### Hybrid NPM Package with Rust Binary

```
starthub-cli/
├── package.json                     # NPM package configuration
├── Cargo.toml                      # Rust package configuration
├── src/                            # Rust source code
│   ├── main.rs                     # Rust CLI entry point
│   ├── commands.rs                 # CLI command implementations
│   ├── config.rs                   # Configuration management
│   ├── models.rs                   # Data models
│   ├── starthub_api.rs             # API client
│   ├── templates.rs                # Code templates
│   ├── ghapp.rs                   # GitHub App integration
│   ├── publish.rs                  # Publishing logic
│   └── runners/                    # Execution runners
│       ├── mod.rs                  # Runner module
│       ├── models.rs               # Runner models
│       ├── local.rs                # Local execution runner
│       └── github.rs               # GitHub Actions runner
├── npm/                            # NPM distribution files
│   └── dist/
│       ├── launch.js               # Node.js launcher script
│       ├── shared-download.js      # Binary download logic
│       └── bin/                    # Downloaded binaries
│           └── starthub            # Platform-specific binary
├── docs/                           # Documentation
│   ├── cli-architecture.md
│   ├── atomic.md
│   ├── composite.md
│   ├── git.md
│   ├── io-definition.md
│   ├── type-checking.md
│   ├── use-cases.md
│   └── wasm-vs-docker.md
├── artifacts/                      # WASM artifacts
│   ├── http-get-wasm_0.0.15.wasm
│   ├── parse-wasm_0.0.9.wasm
│   └── stringify-wasm_0.0.2.wasm
├── test/                           # Test compositions
│   ├── Dockerfile
│   ├── entrypoint.sh
│   ├── starthub.json
│   └── starthub.lock.json
├── ui/                            # Web UI
│   └── dist/
│       ├── index.html
│       ├── assets/
│       └── favicon.ico
└── scripts/
    └── sync-cargo-version.js       # Version synchronization
```

## CLI Layer (Rust)

### Responsibilities

- **Command Parsing**: Parse user commands and arguments using Clap
- **User Interaction**: Provide feedback and error messages
- **Composition Execution**: Execute compositions locally or on GitHub Actions
- **WASM Execution**: Run WASM modules with optimal performance
- **Docker Management**: Create, start, stop, and monitor Docker containers
- **API Communication**: Communicate with StartHub API
- **Configuration**: Manage user settings and authentication

### Implementation

```rust
// CLI handles user interaction and execution
use clap::{Parser, Subcommand};
use anyhow::Result;

#[derive(Parser)]
#[command(name = "starthub")]
#[command(about = "StartHub CLI for executing compositions")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Execute a composition locally
    Run {
        #[arg(short, long)]
        composition: String,
        #[arg(short, long)]
        inputs: Option<String>,
    },
    /// Deploy a composition to GitHub Actions
    Deploy {
        #[arg(short, long)]
        composition: String,
        #[arg(short, long)]
        repository: String,
    },
    /// Initialize a new composition
    Init {
        #[arg(short, long)]
        name: String,
    },
    /// Publish a composition
    Publish {
        #[arg(short, long)]
        composition: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    
    match cli.command {
        Commands::Run { composition, inputs } => {
            execute_composition(composition, inputs).await?;
        }
        Commands::Deploy { composition, repository } => {
            deploy_composition(composition, repository).await?;
        }
        Commands::Init { name } => {
            initialize_composition(name).await?;
        }
        Commands::Publish { composition } => {
            publish_composition(composition).await?;
        }
    }
    
    Ok(())
}
```

### Execution Runners

```rust
// src/runners/mod.rs
pub mod local;
pub mod github;
pub mod models;

use async_trait::async_trait;

#[async_trait]
pub trait Runner {
    async fn execute(&self, composition: &Composition) -> Result<ExecutionResult>;
}

// Local execution runner
pub struct LocalRunner {
    docker_client: DockerClient,
    wasm_executor: WasmExecutor,
}

// GitHub Actions runner
pub struct GitHubRunner {
    github_client: GitHubClient,
    repository: String,
}
```

## Execution Layer (Rust)

### Responsibilities

- **Docker Management**: Create, start, stop, and monitor Docker containers
- **WASM Execution**: Run WASM modules with optimal performance using Wasmtime
- **Composition Parsing**: Parse and validate composition files
- **Resource Management**: Handle memory, CPU, and container lifecycle
- **API Communication**: Communicate with StartHub API for publishing
- **GitHub Integration**: Deploy compositions to GitHub Actions

### Implementation

```rust
// src/runners/local.rs - Local execution runner
use wasmtime::*;
use tokio::process::Command;

pub struct LocalRunner {
    wasm_engine: Engine,
    docker_available: bool,
}

impl LocalRunner {
    pub async fn new() -> Self {
        let wasm_engine = Engine::default();
        let docker_available = Self::check_docker_availability().await;
        
        Self {
            wasm_engine,
            docker_available,
        }
    }

    pub async fn execute_composition(&self, composition: &Composition) -> Result<ExecutionResult> {
        match composition.kind {
            CompositionKind::Docker => {
                if self.docker_available {
                    self.execute_docker_container(composition).await
                } else {
                    Err(anyhow::anyhow!("Docker not available"))
                }
            }
            CompositionKind::Wasm => {
                self.execute_wasm_module(composition).await
            }
        }
    }

    async fn execute_docker_container(&self, composition: &Composition) -> Result<ExecutionResult> {
        let container_name = format!("starthub-{}", uuid::Uuid::new_v4());
        
        // Create and start Docker container
        let output = Command::new("docker")
            .args(&[
                "run",
                "--rm",
                "--name", &container_name,
                &composition.image,
            ])
            .args(&composition.command)
            .output()
            .await?;

        Ok(ExecutionResult {
            exit_code: output.status.code().unwrap_or(1),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        })
    }

    async fn execute_wasm_module(&self, composition: &Composition) -> Result<ExecutionResult> {
        let module = Module::from_file(&self.wasm_engine, &composition.wasm_path)?;
        let mut store = Store::new(&self.wasm_engine, ());
        let instance = Instance::new(&mut store, &module, &[])?;

        // Execute WASM function
        let func = instance.get_typed_func::<(), i32>(&mut store, "main")?;
        let result = func.call(&mut store, ())?;

        Ok(ExecutionResult {
            exit_code: result as i32,
            stdout: format!("WASM execution result: {}", result),
            stderr: String::new(),
        })
    }
}
```

## Distribution Strategy

### NPM Package with Rust Binary

The CLI uses a hybrid distribution approach:

1. **NPM Package**: Contains Node.js launcher and download logic
2. **Rust Binary**: Pre-compiled binary for each platform
3. **Automatic Download**: Binary downloaded on first use

### Package Configuration

```json
{
  "name": "@starthub/cli",
  "version": "0.1.4",
  "bin": {
    "starthub": "npm/dist/launch.js"
  },
  "files": [
    "npm/dist/launch.js",
    "npm/dist/shared-download.js"
  ]
}
```

### Binary Download Logic

```javascript
// npm/dist/launch.js
const { ensureBinary } = require("./shared-download");

const exe = process.platform === "win32" ? "starthub.exe" : "starthub";
const binDir = path.join(__dirname, "bin");
const binPath = path.join(binDir, exe);

// Download binary if missing
if (!fs.existsSync(binPath)) {
  await ensureBinary({ binDir, exe });
}

// Execute Rust binary
const result = spawnSync(binPath, process.argv.slice(2), { stdio: "inherit" });
process.exit(result.status ?? 1);
```

### Platform Support

```javascript
// npm/dist/shared-download.js
function rustTarget() {
  const p = process.platform;
  const a = process.arch;

  if (p === "darwin" && a === "x64")   return "x86_64-apple-darwin";
  if (p === "darwin" && a === "arm64") return "aarch64-apple-darwin";
  if (p === "linux" && a === "x64")    return "x86_64-unknown-linux-gnu";
  if (p === "win32" && a === "x64")    return "x86_64-pc-windows-msvc";

  throw new Error(`Unsupported platform: ${p} ${a}`);
}
```

## Build Process

### Rust Build Configuration

```toml
# Cargo.toml
[package]
name = "starthub"
version = "0.1.4"
edition = "2021"

[[bin]]
name = "starthub"
path = "src/main.rs"

[dependencies]
clap = { version = "4.5", features = ["derive"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread", "process"] }
wasmtime = "22.0"
wasmtime-wasi = "22.0"
reqwest = { version = "0.12", features = ["json", "rustls-tls"] }
serde = { version = "1", features = ["derive"] }
anyhow = "1"
```

### Version Synchronization

```javascript
// scripts/sync-cargo-version.js
const fs = require('fs');
const path = require('path');

const packageJson = JSON.parse(fs.readFileSync('package.json', 'utf8'));
const cargoToml = fs.readFileSync('Cargo.toml', 'utf8');

// Update Cargo.toml version to match package.json
const updatedCargoToml = cargoToml.replace(
  /version = "[\d.]+"/,
  `version = "${packageJson.version}"`
);

fs.writeFileSync('Cargo.toml', updatedCargoToml);
console.log(`Updated Cargo.toml version to ${packageJson.version}`);
```

### GitHub Actions Build

```yaml
# .github/workflows/build.yml
name: Build and Release

on:
  push:
    tags: ['v*']

jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v3
      - uses: actions-rs/toolchain@v1
        with:
          toolchain: stable
          target: x86_64-unknown-linux-gnu
      
      - name: Build Rust binary
        run: cargo build --release --target x86_64-unknown-linux-gnu
      
      - name: Create release archive
        run: |
          tar -czf starthub-v${{ github.ref_name }}-x86_64-unknown-linux-gnu.tar.gz \
            -C target/x86_64-unknown-linux-gnu/release starthub
```

## Usage Patterns

### Local Execution

```bash
# Execute composition locally
starthub run my-composition.json
# - CLI parses composition
# - CLI executes steps locally
# - CLI handles Docker containers
# - CLI handles WASM modules
```

### GitHub Actions Deployment

```bash
# Deploy composition to GitHub Actions
starthub deploy my-composition.json --repository user/repo
# - CLI creates GitHub Actions workflow
# - CLI pushes to repository
# - GitHub Actions executes composition
```

### Composition Management

```bash
# Initialize new composition
starthub init my-composition
# - CLI creates composition template
# - CLI sets up project structure

# Publish composition
starthub publish my-composition.json
# - CLI validates composition
# - CLI uploads to StartHub API
# - CLI creates public composition
```

### Development Workflow

```bash
# Test composition locally
starthub run test-composition.json

# Deploy to GitHub Actions
starthub deploy test-composition.json --repository user/repo

# Publish to StartHub
starthub publish test-composition.json
```

## Summary

The StartHub CLI architecture provides:

- **Optimal performance**: Rust for all operations with near-native speed
- **Single binary**: No dependency on Node.js runtime
- **Easy distribution**: NPM package with automatic binary download
- **Platform support**: Works on Linux, macOS, and Windows
- **Security**: Secure container execution with resource limits
- **Maintainability**: Single codebase with clear separation of concerns
- **Version management**: Synchronized versioning between NPM and Rust

This architecture enables StartHub to provide a powerful, reliable, and user-friendly CLI while maintaining excellent performance for container orchestration and WASM execution. The hybrid NPM + Rust approach combines the best of both worlds: easy distribution through NPM and optimal performance through Rust.
