# Starthub CLI

A powerful command-line tool for building, publishing, and deploying cloud-native applications and WebAssembly modules. Starthub CLI simplifies the process of creating, packaging, and distributing applications with support for both Docker containers and WebAssembly modules.

## Features

- **Multi-format Support**: Build and publish both Docker containers and WebAssembly (WASM) modules
- **Smart Scaffolding**: Automatically generate project structure, Dockerfiles, and configuration files
- **OCI Integration**: Seamless integration with OCI registries (Docker Hub, GitHub Container Registry, etc.)
- **GitHub Actions**: Deploy applications directly to GitHub repositories with automated workflows
- **Local Development**: Support for local deployment and testing
- **Secret Management**: Secure handling of environment variables and secrets during deployment

## Installation

### From Source (Recommended)

```bash
git clone https://github.com/starthubhq/cli.git
cd cli
cargo build --release
```

The binary will be available at `target/release/starthub`.

### From NPM

```bash
npm install -g @starthub/cli
```

## Prerequisites

- **Rust** (for building from source)
- **Docker** (for Docker-based projects)
- **Cargo** (for WASM projects)
- **GitHub account** (for GitHub Actions deployment)

## Authentication

The Starthub CLI requires authentication to access protected resources and publish actions. Before using commands like `publish` or `run`, you'll need to authenticate:

```bash
# Login to Starthub backend
starthub login

# Check authentication status
starthub auth

# Logout when done
starthub logout
```

**Authentication Features:**
- Secure credential storage in user config directory
- Support for custom API endpoints
- Automatic token validation
- Clean logout functionality

## Quick Start

### 1. Initialize a New Project

```bash
starthub init --path my-project
```

This interactive command will:
- Prompt for package details (name, version, type)
- Choose between Docker or WASM project types
- Set up repository and image configurations
- Generate appropriate project structure
- Create necessary configuration files

### 2. Build and Publish

```bash
# Build and publish (Docker or WASM)
starthub publish

# Skip build, only push existing image
starthub publish --no-build
```

### 3. Deploy to GitHub Actions

```bash
starthub run my-package \
  --env production \
  --runner github \
  -e API_KEY=your-secret \
  -e DATABASE_URL=your-db-url
```

## Commands

### `starthub init`

Initialize a new Starthub project with interactive prompts.

```bash
starthub init [--path <PATH>]
```

**Options:**
- `--path <PATH>`: Directory to initialize (default: current directory)

**What it creates:**
- `starthub.json` - Project manifest
- `.gitignore` - Git ignore file
- `.dockerignore` - Docker ignore file (Docker projects only)
- `README.md` - Project documentation
- `Dockerfile` + `entrypoint.sh` (Docker projects only)

### `starthub publish`

Build and publish your application to an OCI registry.

```bash
starthub publish [--no-build]
```

### `starthub login`

Authenticate with the Starthub backend to access protected resources.

```bash
starthub login [--api-base <API_BASE>]
```

**Options:**
- `--api-base <API_BASE>`: Starthub API base URL (default: https://api.starthub.so)

**What it does:**
- Prompts for email and password
- Authenticates against Starthub backend
- Stores access token securely in user config directory
- Supports custom API endpoints for different environments

### `starthub logout`

Logout and remove stored authentication credentials.

```bash
starthub logout
```

**What it does:**
- Removes stored access token
- Clears authentication state
- Safe to run even when not logged in

### `starthub auth`

Check current authentication status and validate stored credentials.

```bash
starthub auth
```

**What it shows:**
- Current authentication status
- API base URL being used
- Token validation results
- Helpful messages for unauthenticated users

**Options:**
- `--no-build`: Skip building, only push existing image/artifact

**For Docker projects:**
- Builds Docker image using local Dockerfile
- Pushes to specified OCI registry
- Generates `starthub.lock.json` with digest

**For WASM projects:**
- Builds WASM module using Cargo
- Pushes to specified OCI registry using ORAS
- Generates `starthub.lock.json` with digest

### `starthub run`

Deploy your application using the specified runner.

```bash
starthub run <ACTION> [OPTIONS]
```

**Arguments:**
- `ACTION`: Package name/action to run (e.g., "my-package")

**Options:**
- `-e, --secret <KEY=VALUE>`: Environment variable or secret (repeatable)
- `--env <ENV>`: Environment name (e.g., "production", "staging")
- `--runner <RUNNER>`: Deployment runner (`github` or `local`, default: `github`)
- `--verbose`: Enable verbose logging

**Examples:**
```bash
# Deploy to GitHub Actions with secrets
starthub run my-app \
  --env production \
  -e DATABASE_URL=postgres://... \
  -e API_KEY=secret123

# Deploy locally
starthub run my-app --runner local
```

### `starthub status`

Check deployment status (currently a placeholder for future implementation).

```bash
starthub status [--id <ID>]
```

## Project Structure

### Manifest File (`starthub.json`)

```json
{
  "name": "my-package",
  "version": "1.0.0",
  "kind": "docker",
  "repository": "github.com/username/my-package",
  "image": "ghcr.io/username/my-package",
  "license": "MIT",
  "inputs": [],
  "outputs": []
}
```

**Supported kinds:**
- `docker`: Docker container applications
- `wasm`: WebAssembly modules

### Lock File (`starthub.lock.json`)

Generated after publishing, contains:
- Package metadata
- OCI digest
- Distribution information

## Runners

### GitHub Runner

- Creates GitHub repository if needed
- Sets up GitHub Actions workflows
- Manages repository secrets
- Dispatches deployment workflows

### Local Runner

- Runs deployments locally
- Useful for testing and development
- No external dependencies

## Configuration

The CLI stores configuration in:
- `~/.starthub/creds/` - Authentication credentials
- `~/.starthub/` - General configuration

## Environment Variables

- `STARTHUB_LOG`: Set logging level (default: `warn`, use `info` for verbose)

## Examples

### Docker Application

```bash
# Initialize
starthub init --path my-docker-app

# Build and publish
starthub publish

# Deploy
starthub run my-docker-app --env production
```

### WASM Module

```bash
# Initialize
starthub init --path my-wasm-module

# Build and publish
starthub publish

# Deploy
starthub run my-wasm-module --runner local
```

## Development

### Building

```bash
cargo build
cargo test
```

### Running Tests

```bash
cargo test
```

## Contributing

1. Fork the repository
2. Create a feature branch
3. Make your changes
4. Add tests
5. Submit a pull request

## License

See [LICENSE](LICENSE) for details.

## Support

- [GitHub Issues](https://github.com/starthubhq/cli/issues)
- [Documentation](https://github.com/starthubhq/cli)