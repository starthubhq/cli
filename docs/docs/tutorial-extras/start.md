---
sidebar_position: 7
---

# start

Start the Starthub server in detached mode.

## Usage

```bash
starthub start [--bind <host:port>]
```

## Options

- `--bind <host:port>` - Host and port to bind the server to (default: `127.0.0.1:3000`)

## Description

The `start` command launches the Starthub server as a background process. The server is required to run actions locally and provides the execution environment for your Starthub actions.

## Prerequisites

Before starting the server, ensure you have:
- **Docker** installed and running
- **Wasmtime** installed

The command will check for these dependencies and display an error if they're missing.

## Server Behavior

- The server runs in the background (detached mode)
- Logs are written to the config directory (`~/.config/starthub/server.log`)
- The server will continue running even after you close the terminal
- Use `starthub stop` to stop the server

## Examples

Start server on default address (127.0.0.1:3000):

```bash
starthub start
```

Start server on a custom address:

```bash
starthub start --bind 0.0.0.0:8080
```

## Output

When successful, the command displays:
- Server start confirmation
- Server URL
- Process ID
- Instructions for viewing logs and stopping the server

## Notes

- The server must be running to use `starthub run` to execute actions
- Use `starthub status` to check if the server is running
- Use `starthub logs` to view server logs
- Use `starthub stop` to stop the server
