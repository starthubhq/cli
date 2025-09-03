# Starthub CLI

A powerful command-line tool for building, publishing, and managing cloud-native applications and WebAssembly modules. Starthub CLI provides a unified workflow for creating, packaging, distributing, and tracking applications with support for both Docker containers and WebAssembly modules.

## 🚀 Features

- **Multi-format Support**: Build and publish both Docker containers and WebAssembly (WASM) modules
- **Smart Scaffolding**: Automatically generate project structure, Dockerfiles, and configuration files
- **Supabase Storage Integration**: Secure artifact storage with S3-compatible API
- **Database Tracking**: Automatic action and version management with duplicate prevention
- **Namespace Isolation**: User-specific namespaces for organized action management
- **Port Management**: Automatic input/output port tracking from manifests
- **Local Development**: Support for local deployment and testing
- **Secret Management**: Secure handling of environment variables and secrets during deployment

## 📦 Installation

### From Source (Recommended)

```bash
git clone https://github.com/starthubhq/cli.git
cd cli
cargo +nightly build --release
```

**Note**: The CLI requires Rust nightly toolchain due to `edition2024` features.

The binary will be available at `target/release/starthub`.

### From NPM

```bash
npm install -g @starthub/cli
```

## 🔧 Prerequisites

- **Rust Nightly** (for building from source - `rustup default nightly`)
- **Docker** (for Docker-based projects)
- **Cargo Component** (for WASM projects - `cargo install cargo-component`)
- **GitHub account** (for GitHub Actions deployment)

## 🔐 Authentication

The Starthub CLI requires authentication to access protected resources and publish actions. The authentication system provides secure, browser-based login with automatic namespace detection.

```bash
# Login to Starthub backend (opens browser)
starthub login

# Check authentication status
starthub auth status

# Logout when done
starthub logout
```

**Authentication Features:**
- **Browser-based login** for enhanced security
- **Automatic namespace detection** based on username
- **Secure credential storage** in user config directory
- **Profile-based access control** with RLS policies
- **Clean logout functionality**

## �� Quick Start

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

# Skip build, only push existing image/artifact
starthub publish --no-build
```

**What happens during publish:**
- ✅ **Builds artifact** (Docker image or WASM module)
- ✅ **Uploads to Supabase Storage** with S3-compatible API
- ✅ **Creates lock file** with metadata and digest
- ✅ **Updates database** with action and version information
- ✅ **Prevents duplicates** - blocks if version already exists

### 3. Deploy to GitHub Actions

```bash
starthub run my-package \
  --env production \
  --runner github \
  -e API_KEY=your-secret \
  -e DATABASE_URL=your-db-url
```

## 📋 Commands

### `starthub init`

Initialize a new Starthub project with interactive prompts.

```bash
starthub init [--path <PATH>]
```

**Options:**
- `--path <PATH>`: Directory to initialize (default: current directory)

**What it creates:**
- `starthub.json` - Project manifest with inputs/outputs
- `.gitignore` - Git ignore file
- `.dockerignore` - Docker ignore file (Docker projects only)
- `README.md` - Project documentation
- `Dockerfile` + `entrypoint.sh` (Docker projects only)

### `starthub publish`

Build and publish your application to Supabase Storage with automatic database tracking.

```bash
starthub publish [--no-build]
```

**Options:**
- `--no-build`: Skip building, only push existing image/artifact

**For Docker projects:**
- Builds Docker image using local Dockerfile
- Uploads to Supabase Storage using AWS SDK
- Generates `starthub.lock.json` with digest
- Updates database with action metadata

**For WASM projects:**
- Builds WASM module using `cargo component build --release`
- Creates ZIP archive of the WASM file
- Uploads to Supabase Storage using AWS SDK
- Generates `starthub.lock.json` with digest
- Updates database with action metadata

### `starthub login`

Authenticate with the Starthub backend using browser-based authentication.

```bash
starthub login
```

**What it does:**
- Opens browser to `https://editor.starthub.so/cli-auth`
- Generates authentication code via backend RPC
- Validates code and stores credentials securely
- Automatically detects user namespace
- Stores profile ID and API configuration

### `starthub logout`

Logout and remove stored authentication credentials.

```bash
starthub logout
```

**What it does:**
- Removes stored access token and profile data
- Clears authentication state
- Safe to run even when not logged in

### `starthub auth status`

Check current authentication status and namespace information.

```bash
starthub auth status
```

**What it shows:**
- Current authentication status
- API base URL being used
- User namespace (derived from username)
- Profile ID and validation results

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

## 🗄️ Database Integration

The CLI automatically tracks all published actions in the database:

### **Action Management:**
- **Automatic creation** of actions in the `actions` table
- **Namespace isolation** using user-specific namespaces
- **Duplicate prevention** - blocks publishing if version already exists

### **Version Tracking:**
- **Version history** stored in `action_versions` table
- **Commit tracking** for version management
- **Automatic linking** to parent actions

### **Port Management:**
- **Input/output ports** automatically extracted from manifests
- **Type mapping** from ShType to database enum values
- **Direction tracking** (INPUT/OUTPUT) for proper port classification

### **Database Schema:**
```sql
-- Actions table
actions (id, name, description, profile_id, rls_owner_id)

-- Action versions
action_versions (id, action_id, version_number, commit_sha, rls_owner_id)

-- Action ports
action_ports (id, action_port_type, action_port_direction, action_version_id, rls_owner_id)

-- Owners (for namespace management)
owners (id, owner_type, profile_id, organization_id, namespace)
```

## 📁 Project Structure

### Manifest File (`starthub.json`)

```json
{
  "name": "my-package",
  "version": "1.0.0",
  "description": "My awesome package",
  "kind": "docker",
  "repository": "github.com/username/my-package",
  "image": "ghcr.io/username/my-package",
  "license": "MIT",
  "inputs": [
    {
      "name": "input1",
      "description": "First input",
      "type": "string",
      "required": true
    }
  ],
  "outputs": [
    {
      "name": "result",
      "description": "Output result",
      "type": "object",
      "required": false
    }
  ]
}
```

**Supported kinds:**
- `docker`: Docker container applications
- `wasm`: WebAssembly modules

### Lock File (`starthub.lock.json`)

Generated after publishing, contains:
- Package metadata and version information
- Distribution URLs for Supabase Storage
- Input/output port definitions
- Digest information for artifact verification

## 🏃 Runners

### GitHub Runner

- Creates GitHub repository if needed
- Sets up GitHub Actions workflows
- Manages repository secrets
- Dispatches deployment workflows

### Local Runner

- Runs deployments locally
- Useful for testing and development
- No external dependencies

## ⚙️ Configuration

The CLI stores configuration in:
- `~/.config/starthub/auth.json` - Authentication credentials and profile data
- `~/.config/starthub/` - General configuration

**Configuration includes:**
- API base URL
- Profile ID for database operations
- User namespace for action isolation
- Authentication tokens

## 🔧 Environment Variables

- `STARTHUB_LOG`: Set logging level (default: `warn`, use `info` for verbose)
- `AWS_ACCESS_KEY_ID`: Supabase Storage S3 access key
- `AWS_SECRET_ACCESS_KEY`: Supabase Storage S3 secret key

## 📚 Examples

### Docker Application

```bash
# Initialize
starthub init --path my-docker-app

# Build and publish (automatically updates database)
starthub publish

# Deploy
starthub run my-docker-app --env production
```

### WASM Module

```bash
# Initialize
starthub init --path my-wasm-module

# Build and publish (automatically updates database)
starthub publish

# Deploy
starthub run my-wasm-module --runner local
```

## 🛠️ Development

### Building

```bash
# Switch to nightly toolchain
rustup default nightly

# Build the CLI
cargo build

# Run tests
cargo test
```

### Architecture

The CLI is built with:
- **Rust** for performance and safety
- **Tokio** for async runtime
- **AWS SDK** for S3-compatible storage
- **Supabase** for backend services
- **PostgreSQL** for action tracking

## 🤝 Contributing

1. Fork the repository
2. Create a feature branch
3. Make your changes
4. Add tests
5. Submit a pull request

## 📄 License

See [LICENSE](LICENSE) for details.

## �� Support

- [GitHub Issues](https://github.com/starthubhq/cli/issues)
- [Documentation](https://github.com/starthubhq/cli)
