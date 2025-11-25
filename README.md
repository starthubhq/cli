# StartHub CLI

The StartHub CLI is a command-line tool for creating, managing, and executing StartHub actions. It provides a complete workflow for developing, publishing, and running actions locally or in the cloud.

## What is StartHub?

StartHub is a platform for building and orchestrating composable actions. Actions are reusable components that can be:
- **WASM-based**: Lightweight WebAssembly modules for fast execution
- **Docker-based**: Containerized applications with full system access
- **Compositions**: Workflows that combine multiple actions together

## Features

- 🚀 **Action Management**: Initialize, build, and publish actions
- 🏃 **Local Execution**: Run actions locally with a built-in server
- 🎨 **Web UI**: Interactive console for configuring and executing actions
- 🔐 **Authentication**: Secure login to StartHub backend
- 📦 **Publishing**: Publish actions to the StartHub registry
- 🔄 **Compositions**: Create and run complex workflows
- 📊 **Logging**: View and follow server logs
- 🧹 **Cache Management**: Clear cached artifacts

## Installation

### From npm (Recommended)

```bash
npm install -g @starthub/cli
```

### From Source

See [Building from Source](#building-from-source) below.

## Quick Start

### 1. Initialize a New Action

```bash
starthub init
```

This creates a `starthub.json` manifest file and project structure based on your chosen action type (WASM, Docker, or Composition).

### 2. Authenticate

```bash
starthub login
```

This opens your browser to authenticate with the StartHub backend.

### 3. Run an Action

```bash
starthub run <action-name>
```

This starts the local server (if not running) and executes the specified action.

### 4. Publish an Action

```bash
starthub publish
```

This builds and publishes your action to the StartHub registry.

## Commands

### Project Management

- `starthub init [--path <path>]` - Initialize a new StartHub project
- `starthub publish [--no-build]` - Publish an action to the registry

### Execution

- `starthub run <action>` - Run an action locally
- `starthub start [--bind <address>]` - Start the server in detached mode
- `starthub stop` - Stop the running server
- `starthub status` - Show server status
- `starthub logs [--follow] [--lines <n>]` - View server logs

### Authentication

- `starthub login [--api-base <url>]` - Authenticate with StartHub backend
- `starthub logout` - Logout from StartHub
- `starthub auth` - Check authentication status

### Utilities

- `starthub reset` - Clear the cache

## Building from Source

### Prerequisites

- **Rust**: Latest stable version (install from [rustup.rs](https://rustup.rs/))
- **Node.js**: Version 20.19+ or 22.12+ (for building the console UI)
- **npm**: Comes with Node.js

### Build Steps

1. **Clone the repository**
   ```bash
   git clone https://github.com/starthubhq/cli.git
   cd cli
   ```

2. **Build the project**
   ```bash
   cargo build --release --workspace
   ```

   This will:
   - Build the CLI binary (`starthub`)
   - Build the server binary (`starthub-server`)
   - Automatically build the console UI (if `server/ui/dist` doesn't exist)
   - Copy the console build output to `server/ui/dist`

3. **Install the binary** (optional)
   ```bash
   cargo install --path .
   ```

### Development Build

For development with faster compile times:

```bash
cargo build --workspace
```

The binaries will be in `target/debug/`:
- `target/debug/starthub` - CLI binary
- `target/debug/starthub-server` - Server binary

### Building the Console UI Separately

The console UI is automatically built during the Rust build process if it doesn't exist. To build it manually:

```bash
cd console
npm install
npm run build
cd ..
cp -r console/dist server/ui/dist
```

## Project Structure

```
cli/
├── src/                    # CLI source code
│   ├── main.rs            # CLI entry point and command definitions
│   ├── commands.rs        # Command implementations
│   ├── publish.rs         # Publishing logic
│   ├── config.rs          # Configuration management
│   ├── models.rs          # Data models (manifests, ports, etc.)
│   ├── templates.rs       # Project templates
│   ├── starthub_api.rs    # API client for StartHub backend
│   └── ghapp.rs           # GitHub App integration
├── server/                # Local server (separate crate)
│   ├── src/               # Server source code
│   └── ui/                # Web UI (Vue.js console)
│       └── dist/          # Built UI (generated, git-ignored)
├── console/               # Vue.js console UI source
│   ├── src/               # Vue components and logic
│   └── dist/              # Build output (copied to server/ui/dist)
├── build.rs               # Build script (builds console UI)
├── Cargo.toml             # Rust workspace configuration
└── package.json           # npm package configuration
```

## Development

### Running Tests

```bash
cargo test
```

### Code Formatting

```bash
cargo fmt
```

### Linting

```bash
cargo clippy
```

### Console UI Development

The console UI is a Vue.js application. To develop it:

```bash
cd console
npm install
npm run dev
```

This starts a development server with hot-reload.

### Server Development

To run the server directly for development:

```bash
cd server
cargo run -- --verbose
```

Or from the root:

```bash
cargo run --bin starthub-server -- --verbose
```

### Environment Variables

- `STARTHUB_LOG` - Log level filter (e.g., `info`, `debug`, `warn`)
- `STARTHUB_API` - API base URL (default: `https://api.starthub.so`)

## Contributing

We welcome contributions! Here's how you can help:

### Getting Started

1. **Fork the repository** and clone your fork
2. **Create a branch** for your feature or bugfix
   ```bash
   git checkout -b feature/your-feature-name
   ```
3. **Make your changes** and test them
4. **Commit your changes** with clear commit messages
5. **Push to your fork** and open a pull request

### Development Guidelines

- **Code Style**: Follow Rust standard formatting (`cargo fmt`)
- **Testing**: Add tests for new functionality
- **Documentation**: Update README and code comments for significant changes
- **Commits**: Write clear, descriptive commit messages
- **PRs**: Keep pull requests focused and well-described

### Areas for Contribution

- 🐛 **Bug Fixes**: Fix issues and improve stability
- ✨ **Features**: Add new functionality
- 📚 **Documentation**: Improve docs and examples
- 🧪 **Tests**: Add test coverage
- 🎨 **UI Improvements**: Enhance the console UI
- ⚡ **Performance**: Optimize execution and build times

### Building Before Submitting

Make sure your changes build successfully:

```bash
# Build everything
cargo build --release --workspace

# Run tests
cargo test

# Check formatting
cargo fmt --check

# Run clippy
cargo clippy -- -D warnings
```

### Console UI Contributions

When contributing to the console UI:

```bash
cd console
npm run lint        # Check linting
npm run type-check  # Check TypeScript types
npm run build       # Ensure build succeeds
```

## Architecture

### CLI Components

- **Commands Module**: Implements all CLI commands
- **Publish Module**: Handles action publishing workflow
- **API Client**: Communicates with StartHub backend
- **Config Module**: Manages local configuration
- **Models**: Defines data structures (manifests, ports, etc.)

### Server Components

The server is a separate Rust binary that:
- Serves the Vue.js console UI
- Provides REST API endpoints for action execution
- Handles WebSocket connections for real-time updates
- Manages action execution and workflow orchestration

See `server/README.md` for detailed server documentation.

## Troubleshooting

### Build Issues

**Console UI build fails:**
- Ensure Node.js version is 20.19+ or 22.12+
- Run `npm install` in the `console/` directory
- Check that all dependencies are installed

**Rust build fails:**
- Ensure you have the latest stable Rust toolchain
- Run `rustup update`
- Check that all workspace members build: `cargo build --workspace`

### Runtime Issues

**Server won't start:**
- Check if port 3000 is already in use
- Use `--bind` to specify a different address
- Check logs with `starthub logs`

**Action execution fails:**
- Verify authentication: `starthub auth`
- Check server status: `starthub status`
- Clear cache: `starthub reset`

## License

See the main project license for details..

## Links

- **Repository**: https://github.com/starthubhq/cli
- **Documentation**: https://docs.starthub.so
- **Website**: https://starthub.so
