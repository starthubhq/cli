---
sidebar_position: 2
---

# init

Initialize a new Starthub project.

## Usage

```bash
starthub init [--path <directory>]
```

## Options

- `--path <directory>` - Directory path where to initialize the project (default: current directory)

## Description

The `init` command creates a new Starthub project by generating a `starthub.json` manifest file and setting up the necessary project structure based on the selected package type.

## Interactive Setup

When you run `init`, you'll be prompted to provide:

1. **Package name** - The name of your package (default: `http-get-wasm`)
2. **Version** - The version of your package (default: `0.1.0`)
3. **Package type** - Choose from:
   - `Wasm` - WebAssembly module
   - `Docker` - Docker container
   - `Composition` - Composite action

4. **Repository** - The repository URL (defaults vary by package type)

## Generated Files

Based on the selected package type, `init` will create:

### Wasm Package
- `starthub.json` - Project manifest
- `Cargo.toml` - Rust project configuration
- `src/main.rs` - Rust source file template

### Docker Package
- `starthub.json` - Project manifest
- `Dockerfile` - Docker configuration template

### Composition Package
- `starthub.json` - Project manifest
- `composition.json` - Composition configuration template

## Example

```bash
starthub init --path ./my-action
```

This will create a new Starthub project in the `./my-action` directory with all necessary files.
