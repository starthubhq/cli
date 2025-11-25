---
sidebar_position: 6
---

# reset

Clear the Starthub cache.

## Usage

```bash
starthub reset
```

## Description

The `reset` command clears the Starthub cache directory. This can be useful if you're experiencing issues with cached artifacts or want to free up disk space.

## What Gets Cleared

The command removes the entire cache directory located at:
- **macOS/Linux**: `~/.cache/starthub/oci/`
- **Windows**: `%LOCALAPPDATA%\starthub\oci\`

This cache typically contains downloaded OCI artifacts and other cached resources.

## Example

```bash
starthub reset
```

## Output

The command will display:
- A message indicating the cache is being cleared
- The path to the cache directory that was removed
- A success message when complete

If the cache directory doesn't exist, you'll see an informational message indicating that.

## Notes

- This operation is safe and will not affect your authentication or configuration
- Cached artifacts will be re-downloaded as needed when you use Starthub commands
- The server does not need to be stopped to clear the cache
