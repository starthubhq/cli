---
sidebar_position: 8
---

# status

Check the status of the Starthub server.

## Usage

```bash
starthub status
```

## Description

The `status` command checks whether the Starthub server is running and provides information about the server process and its health.

## Information Displayed

The command displays:

1. **Server Process Status** - Whether server processes are running
2. **Process Details** - Process IDs and command information for each running server process
3. **HTTP Response Status** - Whether the server is responding to HTTP requests
4. **Server URL** - The URL where the server is accessible
5. **Log File Location** - Path to the server log file and its size

## Example Output

```
📊 Checking server status...
✅ Server is running
📋 Found 1 server process(es):
  - PID: 12345 | Command: starthub-server --bind 127.0.0.1:3000
🌐 Server is responding at http://127.0.0.1:3000
📝 Log file: /Users/username/.config/starthub/server.log (12345 bytes)
```

## When Server is Not Running

If the server is not running, you'll see:

```
📊 Checking server status...
❌ Server is not running
💡 Start the server with 'starthub start'
```

## Use Cases

- Verify the server is running before executing actions
- Troubleshoot server issues
- Check server health and responsiveness
- Find the log file location

## Related Commands

- `starthub start` - Start the server
- `starthub stop` - Stop the server
- `starthub logs` - View server logs
