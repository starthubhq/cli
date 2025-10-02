<<<<<<< HEAD
# Starthub CLI

A powerful command-line tool for building, publishing, and managing cloud-native applications and WebAssembly modules. Starthub CLI provides a unified workflow for creating, packaging, distributing, and tracking applications with support for both Docker containers and WebAssembly modules.
=======
# StartHub CLI

A powerful command-line tool for building, publishing, and managing cloud-native applications and WebAssembly modules. The StartHub CLI uses a hybrid architecture with a lightweight CLI and a separate server for optimal performance and scalability.

## 🏗️ Architecture

The StartHub CLI follows a **two-tier architecture**:

- **CLI Layer (Rust)**: Handles user interaction, command parsing, and server management
- **Server Layer (Rust)**: Manages execution, Docker containers, WASM modules, and real-time communication

```
┌─────────────────┐    ┌─────────────────┐    ┌─────────────────┐
│   User Input    │───▶│   CLI Process   │───▶│ Server Process  │
│                 │    │                 │    │                 │
│ • Commands      │    │ • Parsing       │    │ • Execution     │
│ • Parameters    │    │ • Validation    │    │ • Docker/WASM   │
│ • Options       │    │ • Server Mgmt   │    │ • WebSocket     │
└─────────────────┘    └─────────────────┘    └─────────────────┘
```
>>>>>>> staging

## 🚀 Features

- **Hybrid Architecture**: Lightweight CLI + powerful server
- **Multi-format Support**: Docker containers and WebAssembly modules
- **Real-time Execution**: WebSocket communication for live updates
- **Smart Scaffolding**: Automatic project structure generation
- **Local Development**: Full local execution with UI
- **Process Management**: Automatic server lifecycle management
- **Browser Integration**: Automatic browser launching for UI

## 📦 Installation

### From Source (Recommended)

```bash
git clone https://github.com/starthubhq/cli.git
cd cli
cargo build --release
```

The binaries will be available at:
- `target/release/starthub` - CLI binary
- `target/release/starthub-server` - Server binary

### From NPM

```bash
npm install -g @starthub/cli
```

## 🔧 Prerequisites

- **Rust** (stable toolchain)
- **Docker** (for Docker-based projects)
- **Wasmtime** (for WASM execution)
- **GitHub account** (for GitHub Actions deployment)

## 🎯 Quick Start

### 1. Initialize a New Project

```bash
starthub init --path my-project
```

This interactive command will:
- Prompt for package details (name, version, type)
- Choose between Docker, WASM, or Composition project types
- Generate appropriate project structure
- Create necessary configuration files

### 2. Run Actions Locally

```bash
# Start server and open browser UI
starthub run my-action

# The CLI will:
# 1. Start the server process
# 2. Open browser to the UI
# 3. Wait for user interaction
# 4. Clean up on exit
```

### 3. Build and Publish

```bash
# Build and publish
starthub publish

# Skip build, only push existing artifacts
starthub publish --no-build
```

## 📋 Commands

### `starthub init`

Initialize a new StartHub project with interactive prompts.

```bash
starthub init [--path <PATH>]
```

**Options:**
- `--path <PATH>`: Directory to initialize (default: current directory)

**What it creates:**
- `starthub.json` - Project manifest with inputs/outputs
- `Cargo.toml` - Rust project configuration (WASM projects)
- `Dockerfile` - Docker configuration (Docker projects)
- `src/main.rs` - Source code template

### `starthub run`

Start the server and open the browser UI for action execution.

```bash
starthub run <ACTION>
```

**Arguments:**
- `ACTION`: Action name to run (e.g., "my-action", "namespace/action@version")

**What it does:**
1. Starts the `starthub-server` process
2. Opens browser to the server UI
3. Waits for user interaction
4. Cleans up server process on exit

### `starthub publish`

Build and publish your application.

```bash
starthub publish [--no-build]
```

**Options:**
- `--no-build`: Skip building, only push existing artifacts

### `starthub login`

Authenticate with the StartHub backend.

```bash
starthub login [--api-base <URL>]
```

**Options:**
- `--api-base <URL>`: StartHub API base URL (default: https://api.starthub.so)

### `starthub logout`

Logout and remove stored credentials.

```bash
starthub logout
```

### `starthub auth`

Check authentication status.

```bash
starthub auth
```

### `starthub status`

Check deployment status.

```bash
starthub status [--id <ID>]
```

**Options:**
- `--id <ID>`: Specific deployment ID to check

## 🖥️ Server Architecture

The StartHub server (`starthub-server`) provides:

### **HTTP API**
- `GET /api/status` - Server health
- `POST /api/run` - Execute actions
- `GET /api/types` - Get action types
- `GET /api/execution-orders` - Get execution orders

### **WebSocket Support**
- Real-time execution updates
- Live progress monitoring
- Error reporting

### **Execution Engine**
- **WASM Execution**: Using Wasmtime runtime
- **Docker Execution**: Container orchestration
- **Composite Actions**: Multi-step workflow execution
- **Artifact Management**: Downloading and caching

### **UI Serving**
- Vue.js frontend
- SPA routing support
- Static asset serving

## 🏃 Running the Server

### Direct Server Execution

```bash
# Run server directly
cd server
cargo run

# With custom options
cargo run -- --bind 0.0.0.0:8080 --verbose
```

### Server Options

- `--bind <ADDRESS>`: Server bind address (default: 127.0.0.1:3000)
- `--verbose, -v`: Enable verbose logging
- `--help`: Show help information

## 📁 Project Structure

```
cli/
├── Cargo.toml              # Workspace configuration
├── src/                    # CLI source code
│   ├── main.rs            # CLI entry point
│   ├── commands.rs        # Command implementations
│   ├── models.rs          # Data models
│   └── templates.rs       # Project templates
└── server/                # Server package
    ├── Cargo.toml         # Server dependencies
    ├── src/
    │   ├── main.rs        # Server entry point
    │   ├── models.rs      # Server data models
    │   └── execution.rs    # Execution engine
    └── README.md          # Server documentation
```

## 🔧 Configuration

### Environment Variables

- `STARTHUB_LOG`: Logging level (`info`, `debug`, `warn`, `error`)
- `STARTHUB_API`: API base URL (default: https://api.starthub.so)
- `STARTHUB_TOKEN`: Authentication token

### Server Configuration

The server can be configured via:
- Command-line arguments
- Environment variables
- Configuration files (future)

## 🚀 Execution Flow

```
1. User runs: starthub run my-action
2. CLI starts server process
3. Server fetches action metadata
4. Server downloads artifacts (WASM/Docker)
5. Server executes action
6. Server sends real-time updates via WebSocket
7. Browser UI displays progress
8. User sees results
9. CLI cleans up server process
```

## 📚 Examples

### Docker Application

```bash
# Initialize
starthub init --path my-docker-app

# Run locally with UI
starthub run my-docker-app

# Publish
starthub publish
```

### WASM Module

```bash
# Initialize
starthub init --path my-wasm-module

# Run locally with UI
starthub run my-wasm-module

# Publish
starthub publish
```

### Composite Action

```bash
# Initialize composition
starthub init --path my-composition

# Run with UI
starthub run my-composition
```

## 🛠️ Development

### Building

```bash
# Build CLI
cargo build

# Build server
cd server && cargo build

# Build both
cargo build --workspace
```

### Testing

```bash
# Test CLI
cargo test

# Test server
cd server && cargo test

# Test workspace
cargo test --workspace
```

### Architecture Benefits

- **Separation of Concerns**: CLI handles UI, server handles execution
- **Scalability**: Server can be deployed independently
- **Performance**: Long-running server process
- **Maintainability**: Clear boundaries between components
- **Real-time Updates**: WebSocket communication

## 🔍 Troubleshooting

### Server Not Starting

```bash
# Check if server binary exists
ls target/debug/starthub-server

# Build server explicitly
cargo build --bin starthub-server

# Run server directly for debugging
cd server && cargo run -- --verbose
```

### Port Conflicts

```bash
# Use different port
cd server && cargo run -- --bind 127.0.0.1:3001
```

### UI Not Loading

- Ensure UI is built in `ui/dist/` directory
- Check server logs for errors
- Verify server is running on correct port

## 🤝 Contributing

1. Fork the repository
2. Create a feature branch
3. Make your changes
4. Add tests
5. Submit a pull request

## 📄 License

See [LICENSE](LICENSE) for details.

## 🆘 Support

- [GitHub Issues](https://github.com/starthubhq/cli/issues)
- [Documentation](https://github.com/starthubhq/cli)
- [Server Documentation](server/README.md)