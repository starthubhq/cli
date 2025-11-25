---
sidebar_position: 3
---

# logs

View server logs.

## Usage

```bash
starthub logs [--follow] [--lines <number>]
```

## Options

- `-f, --follow` - Follow log output (like `tail -f`)
- `-l, --lines <number>` - Number of lines to show from the end (default: 100)

## Description

The `logs` command displays the server logs from the Starthub server process. By default, it shows the last 100 lines of logs. You can use the `--follow` flag to continuously stream new log entries as they are written.

## Examples

Show the last 100 lines of logs:

```bash
starthub logs
```

Show the last 50 lines:

```bash
starthub logs --lines 50
```

Follow logs in real-time:

```bash
starthub logs --follow
```

Follow logs showing only the last 20 lines initially:

```bash
starthub logs --follow --lines 20
```

## Log File Location

Logs are stored in your system's config directory:
- **macOS/Linux**: `~/.config/starthub/server.log`
- **Windows**: `%APPDATA%\starthub\server.log`

## Notes

- If the server is not running, you'll see a message indicating the log file was not found
- Use `Ctrl+C` to stop following logs when using the `--follow` flag
- The server must be started with `starthub start` before logs will be available
