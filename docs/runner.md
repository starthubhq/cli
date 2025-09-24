# StartHub Runner Architecture

The StartHub Runner is the execution engine responsible for resolving, downloading, and executing composite actions. It implements a recursive dependency resolution system that breaks down complex workflows into atomic execution units.

## Overview

The runner follows a **recursive dependency resolution** pattern:

1. **Download** the `starthub-lock.json` for the target action
2. **Parse** the lock file to extract steps and their dependencies  
3. **Recursively resolve** each step until reaching atomic dependencies (WASM/Docker)
4. **Execute** the resolved dependency graph in the correct order

## Architecture Flow

```
┌─────────────────┐    ┌─────────────────┐    ┌─────────────────┐
│   Action Ref    │───▶│  Download Lock  │───▶│ Parse Lock File │
│  (user input)   │    │     File        │    │   (steps)       │
└─────────────────┘    └─────────────────┘    └─────────────────┘
                                                         │
                                                         ▼
┌─────────────────┐    ┌─────────────────┐    ┌─────────────────┐
│   Execute Plan  │◀───│ Build Execution │◀───│ Resolve Dependencies │
│  (WASM/Docker)  │    │      Plan       │    │   (recursive)   │
└─────────────────┘    └─────────────────┘    └─────────────────┘
```

## Core Components

### 1. Execution Engine
The main orchestrator that coordinates the entire execution process. It manages the API client for downloading artifacts and maintains a local cache directory for downloaded components.

### 2. Dependency Resolution
Recursively resolves all dependencies by following this algorithm:

1. **Check for circular dependencies** - Prevent infinite recursion
2. **Download action metadata** - Get basic information about the action
3. **Construct storage URL** - Build the path to the starthub-lock.json file in the artifacts folder
4. **Download and parse starthub-lock.json** - Get the action lock file
5. **Recursively resolve each step** - Continue until all dependencies are atomic

### 3. Execution Plan
Represents the resolved dependency graph ready for execution. It contains an ordered list of execution steps and working directory configuration.

## Execution Process

### Phase 1: Dependency Resolution

The runner starts with a composite action and recursively resolves all its dependencies:

```
Action: "my-composite-action@1.0.0"
├── Step 1: "data-processing@2.1.0" (composition)
│   ├── Step 1.1: "validate-input@1.0.0" (wasm)
│   ├── Step 1.2: "transform-data@1.5.0" (docker)
│   └── Step 1.3: "save-results@1.0.0" (wasm)
├── Step 2: "send-notification@1.0.0" (docker)
└── Step 3: "cleanup@1.0.0" (wasm)
```

**Resolution Process:**
1. Download `starthub-lock.json` for the main action
2. Parse lock file to find all steps
3. For each step:
   - If `kind == "composition"` → recursively resolve
   - If `kind == "wasm"` or `"docker"` → atomic dependency found
4. Continue until all dependencies are atomic

### Phase 2: Artifact Prefetching

Before execution, all required artifacts are downloaded and cached:

- **Docker images** are pulled using `docker pull`
- **WASM modules** are downloaded to the local cache
- **Lock files** are stored in memory for quick access

### Phase 3: Sequential Execution

Execute steps in dependency order with data pipelining:

1. **Initialize data** - Start with initial JSON input
2. **Execute each step** - Run WASM or Docker containers
3. **Pipe data** - Pass stdout from one step to stdin of the next
4. **Collect output** - Capture final JSON result from last step
5. **Return result** - Provide complete execution output

## Execution Types

### WASM Execution
- Uses wasmtime runtime for sandboxed execution
- Provides HTTP capabilities and environment variables
- Executes in isolated environment with no filesystem access
- Reads JSON from stdin and writes JSON to stdout

### Docker Execution
- Runs containers with network isolation by default
- Supports volume mounts and environment variables
- Configurable networking (bridge, none, host)
- Reads JSON from stdin and writes JSON to stdout
- Executes with proper resource limits

## Communication Model

### Stdin/Stdout Communication
Each step communicates through standard input and output streams:

**Input to each step:**
- **Stdin**: JSON data from previous steps or initial inputs
- **Environment variables**: Step-specific configuration

**Output from each step:**
- **Stdout**: JSON data that becomes input for subsequent steps
- **Stderr**: Logging and error information

### Data Flow
The system uses a simple pipe model:
- Each step reads JSON from stdin
- Processes the data according to its logic
- Writes JSON result to stdout
- Next step receives the output as its stdin

## Error Handling

### Circular Dependency Detection
- Maintains a visited set to prevent infinite recursion
- Skips already processed dependencies
- Provides clear error messages for circular references

### Step Failure Handling
- Captures exit codes from failed steps
- Provides detailed error information
- Stops execution on critical failures

### Artifact Download Failures
- Gracefully handles missing artifacts
- Skips unavailable dependencies
- Continues execution with available components

## Caching Strategy

### Local Cache Directory
- Uses system cache directory (e.g., `~/.cache/starthub/oci`)
- Falls back to temporary directory if cache unavailable
- Creates directory structure automatically

### Artifact Persistence
- **WASM modules**: Cached as `.wasm` files
- **Docker images**: Cached by Docker daemon
- **Lock files**: Cached in memory during execution
- **State**: Accumulated in memory, not persisted

## Performance Optimizations

### Parallel Prefetching
- Downloads all artifacts concurrently
- Reduces total execution time
- Maximizes network utilization

### Incremental Resolution
- Only resolves dependencies that haven't been visited
- Caches resolved lock files in memory
- Skips already downloaded artifacts

## Security Considerations

### Network Isolation
- Docker containers run with no network by default
- WASM modules have limited network access
- Configurable network policies per step

### Resource Limits
- Process isolation via Docker containers
- WASM sandboxing via wasmtime
- No direct filesystem access for WASM modules

### Input Validation
- All inputs are validated against manifest schemas
- Environment variables are sanitized
- Mount paths are validated and absolutized

## Example Execution Flow

```
1. User runs: starthub run my-workflow@1.0.0
2. Runner downloads: my-workflow@1.0.0/starthub-lock.json
3. Parses lock file → finds 3 steps
4. Resolves step dependencies:
   - step1: data-processor@2.0.0 (composition)
     - step1.1: validator@1.0.0 (wasm)
     - step1.2: transformer@1.5.0 (docker)
   - step2: notifier@1.0.0 (docker)
   - step3: cleaner@1.0.0 (wasm)
5. Prefetches all artifacts
6. Executes in order:
   - validator@1.0.0 (stdin: initial_data) → stdout: validated_data
   - transformer@1.5.0 (stdin: validated_data) → stdout: transformed_data
   - notifier@1.0.0 (stdin: transformed_data) → stdout: notification_result
   - cleaner@1.0.0 (stdin: notification_result) → stdout: final_result
7. Returns final JSON result
```

This architecture provides a robust, scalable system for executing complex workflows while maintaining security, performance, and reliability.