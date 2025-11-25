---
sidebar_position: 9
---

# stop

Stop the running Starthub server.

## Usage

```bash
starthub stop
```

## Description

The `stop` command gracefully stops all running Starthub server processes. It finds all `starthub-server` processes and terminates them.

## Behavior

- The command searches for all running `starthub-server` processes
- Processes are terminated gracefully using the TERM signal (Unix) or taskkill (Windows)
- The command reports how many processes were stopped

## Example Output

When server processes are found and stopped:

```
🛑 Stopping StartHub server...
🔍 Found starthub-server process: PID 12345
✅ Killed process 12345
✅ Stopped 1 server process(es)
```

When no server processes are running:

```
🛑 Stopping StartHub server...
ℹ️  No running StartHub server processes found
```

## Platform-Specific Behavior

### Unix/Linux/macOS
- Uses `ps` to find processes
- Sends `TERM` signal to gracefully stop processes

### Windows
- Uses `tasklist` to find processes
- Uses `taskkill` with `/F` flag to force stop processes

## Notes

- The command will stop all running server processes
- Any actions currently executing may be interrupted
- The server can be restarted using `starthub start`
- Use `starthub status` to verify the server has stopped

## Related Commands

- `starthub start` - Start the server
- `starthub status` - Check server status
- `starthub logs` - View server logs
