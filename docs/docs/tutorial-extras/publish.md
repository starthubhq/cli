---
sidebar_position: 5
---

# publish

Publish an action to the Starthub registry.

## Usage

```bash
starthub publish [--no-build]
```

## Options

- `--no-build` - Skip the build step and only push/tag (assumes the image or WASM file already exists locally)

## Description

The `publish` command builds and publishes your action to the Starthub registry. The command reads the `starthub.json` manifest file in the current directory to determine the package type and publishing process.

## Package Types

### Docker Package

For Docker packages, the command will:

1. Build the Docker image (unless `--no-build` is specified)
2. Tag the image for the Starthub registry (`registry.starthub.so/<name>:<version>`)
3. Push the image to the registry

**Requirements:**
- A `Dockerfile` must exist in the current directory
- Docker must be installed and running

### Wasm Package

For WebAssembly packages, the command will:

1. Build the WASM module using `cargo build --release --target wasm32-wasi` (unless `--no-build` is specified)
2. Package the WASM file into a zip archive
3. Prepare it for upload to the registry

**Requirements:**
- Rust and Cargo must be installed
- The project must be a valid Rust project targeting `wasm32-wasi`

### Composition Package

Composition actions cannot be published directly using this command.

## Examples

Publish with build:

```bash
starthub publish
```

Publish without building (assumes image/WASM already exists):

```bash
starthub publish --no-build
```

## Notes

- The command must be run in a directory containing a `starthub.json` file
- You must be authenticated using `starthub login` before publishing
- The package name and version are read from the `starthub.json` manifest
