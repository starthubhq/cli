# Starthub CLI

Starthub CLI is a command-line tool for initializing, deploying, and managing projects using Starthub templates and automation, primarily with GitHub integration.

## Features

- **Initialize Projects:** Quickly bootstrap a new project configuration.
- **Deploy from Templates:** Create new repositories using predefined templates (e.g., "chirpstack") and optionally set up secrets and environment variables.
- **Manage Secrets:** Pass secrets and environment variables securely during deployment.
- **Check Status:** View deployment status and progress.
- **GitHub Integration:** Authenticates and interacts directly with your GitHub account to create and manage repositories.

## Requirements

- Rust (for building from source)
- GitHub account for authentication and repository management

## Installation

Clone the repository and build using Cargo:

```bash
git clone https://github.com/starthubhq/cli.git
cd cli
cargo build --release
```

Alternatively, use the prebuilt binaries if available.

## Usage

Run the CLI with the following commands:

### Initialize a New Project

```bash
starthub init --path .
```

Creates a new project configuration in the specified path.

### Deploy a Template

```bash
starthub deploy chirpstack -e KEY=VALUE --env production --runner github
```

- `chirpstack`: Name of the package/template to deploy
- `-e KEY=VALUE`: Pass secrets or environment variables (repeatable)
- `--env`: Specify an environment name
- `--runner`: Select deployment runner (`github` or `local`)

### Check Deployment Status

```bash
starthub status --id <deployment-id>
```

Displays the status of the specified deployment.

### Verbose Logging

Add `--verbose` to any command for more detailed output.

## Authentication

On first use, you'll be prompted to authenticate with GitHub. The CLI uses device login flow for secure authorization. You can trigger login manually if needed:

```bash
starthub login --runner github
```

Follow the printed instructions to complete authorization in your browser.

## Configuration

The CLI stores configuration and credentials in your home directory at `~/.starthub/creds/`.

## Templates

Currently supported templates:
- `chirpstack`

You can add more templates by contributing to the project.

## Contributing

Contributions are welcome! Please open issues and pull requests on [GitHub](https://github.com/starthubhq/cli).

## License

See [LICENSE](LICENSE) for details.

## Contact

For support and questions, reach out via [GitHub Issues](https://github.com/starthubhq/cli/issues).