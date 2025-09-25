# StartHub Server

The StartHub Server is a standalone Rust web server that provides the backend infrastructure for the StartHub CLI. It handles HTTP/WebSocket communication, serves the UI, and manages action execution workflows.

## Overview

The server is designed to run as a long-running process, started by the StartHub CLI when needed. It provides:

- **Web UI Serving**: Serves the Vue.js frontend for action configuration and execution
- **REST API**: Provides endpoints for action metadata, types, and execution orders
- **WebSocket Support**: Real-time communication for execution status and updates
- **Action Execution**: Handles the execution of StartHub actions and compositions

## Architecture

### Core Components

- **HTTP Server**: Built on Axum for high-performance async HTTP handling
- **WebSocket Handler**: Real-time bidirectional communication
- **State Management**: In-memory storage for types, execution orders, and composition data
- **UI Serving**: Static file serving with SPA fallback for Vue Router

### Key Features

- **Modular Design**: Clean separation from CLI concerns
- **Async Processing**: Built on Tokio for high concurrency
- **CORS Support**: Permissive CORS for development
- **Logging**: Structured logging with configurable levels
- **Process Management**: Designed to be spawned and managed by the CLI

## API Endpoints

### Status
- `GET /api/status` - Server health and status

### Actions
- `POST /api/action` - Handle action requests
- `POST /api/run` - Execute actions with inputs

### Types
- `GET /api/types` - Get all stored types
- `GET /api/types/:action` - Get types for specific action

### Execution Orders
- `GET /api/execution-orders` - Get all execution orders
- `GET /api/execution-orders/:action` - Get execution order for specific action

### WebSocket
- `GET /ws` - WebSocket connection for real-time updates

### UI
- `GET /` - Serve main application
- `GET /assets/*` - Serve static assets
- `GET /*` - SPA fallback for Vue Router

## Usage

### Development

```bash
# Build the server
cargo build

# Run with verbose logging
cargo run -- --verbose

# Run on custom host/port
cargo run -- --bind 0.0.0.0:8080
```

### Production

The server is typically started by the StartHub CLI:

```bash
# CLI automatically starts the server
starthub run my-action
```

### Command Line Options

- `--bind <ADDRESS>`: Server bind address (default: `127.0.0.1:3000`)
- `--verbose, -v`: Enable verbose logging
- `--help`: Show help information

## Configuration

### Environment Variables

- `STARTHUB_LOG`: Log level filter (e.g., `info`, `debug`, `warn`)
- `RUST_LOG`: Alternative log level configuration

### Logging

The server uses structured logging with the following levels:
- `ERROR`: Critical errors
- `WARN`: Warnings and non-critical issues
- `INFO`: General information (enabled with `--verbose`)
- `DEBUG`: Detailed debugging information

## State Management

The server maintains several types of state:

### Types Storage
- Stores type definitions for actions
- Key format: `{action_ref}:{type_name}`
- Used for input/output validation

### Execution Orders
- Tracks the execution order of composite actions
- Used for dependency resolution

### Composition Data
- Stores composition manifests
- Used for action orchestration

## WebSocket Protocol

The WebSocket connection provides real-time updates:

### Message Types

```json
{
  "type": "connection",
  "message": "Connected to Starthub WebSocket server",
  "timestamp": "2024-01-01T00:00:00Z"
}
```

```json
{
  "type": "echo",
  "message": "user message",
  "timestamp": "2024-01-01T00:00:00Z"
}
```

### Connection Flow

1. Client connects to `/ws`
2. Server sends welcome message
3. Bidirectional communication established
4. Server forwards broadcast messages to client
5. Client can send messages for echo testing

## Dependencies

### Core Dependencies
- **axum**: HTTP server framework
- **tokio**: Async runtime
- **serde**: Serialization/deserialization
- **tracing**: Structured logging

### WebSocket
- **futures-util**: Stream utilities
- **tokio-sync**: Async synchronization primitives

### HTTP
- **tower**: Middleware framework
- **tower-http**: HTTP middleware (CORS, compression, etc.)

### Utilities
- **chrono**: Date/time handling
- **clap**: Command-line argument parsing
- **anyhow**: Error handling

## Development

### Project Structure

```
server/
├── Cargo.toml          # Dependencies and metadata
├── README.md           # This file
└── src/
    └── main.rs         # Server implementation
```

### Building

```bash
# Debug build
cargo build

# Release build
cargo build --release

# Check for issues
cargo check

# Run tests
cargo test
```

### Code Organization

- **Main Function**: CLI parsing and server startup
- **AppState**: Shared state management
- **Route Handlers**: HTTP endpoint implementations
- **WebSocket Handler**: Real-time communication
- **Utility Functions**: Helper functions for common operations

## Integration with CLI

The server is designed to be managed by the StartHub CLI:

1. **Process Spawning**: CLI starts server as child process
2. **Lifecycle Management**: CLI handles start/stop/restart
3. **Communication**: CLI communicates via HTTP/WebSocket
4. **Resource Management**: CLI manages server resources

## Security Considerations

- **CORS**: Permissive CORS for development (should be restricted in production)
- **Input Validation**: All inputs should be validated
- **Rate Limiting**: Consider implementing rate limiting for production
- **Authentication**: No authentication currently implemented

## Performance

- **Async I/O**: Non-blocking I/O for high concurrency
- **Memory Efficiency**: In-memory state with reasonable limits
- **Connection Pooling**: Efficient connection management
- **Resource Limits**: Consider implementing resource limits for production

## Troubleshooting

### Common Issues

1. **Port Already in Use**
   ```bash
   # Check what's using the port
   lsof -i :3000
   
   # Use different port
   cargo run -- --bind 127.0.0.1:3001
   ```

2. **UI Not Found**
   - Ensure UI is built in `ui/dist/` directory
   - Check file permissions

3. **WebSocket Connection Issues**
   - Check firewall settings
   - Verify CORS configuration

### Debug Mode

```bash
# Enable debug logging
RUST_LOG=debug cargo run -- --verbose
```

## Contributing

1. Follow Rust coding standards
2. Add tests for new functionality
3. Update documentation for API changes
4. Consider performance implications
5. Test with CLI integration

## License

Part of the StartHub project. See main project license for details.
