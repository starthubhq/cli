# Composition ID Support

The Starthub CLI now supports running compositions directly using their ID. This allows you to execute compositions stored in Supabase storage without needing to download the `starthub.json` file locally.

## How it works

When you provide a composition ID to the CLI, it will:

1. **Detect the composition ID** - The CLI recognizes composition IDs as simple identifiers (no special characters like `{}`, `/`, `:`, `@`, etc.)
2. **Fetch from Supabase storage** - It fetches the `starthub.json` file from the Supabase storage bucket at `/storage/v1/object/public/compositions/{composition_id}/starthub.json`
3. **Parse and execute** - The composition is parsed and executed using the existing local runner infrastructure

## Usage

### Basic usage

```bash
# Run a composition using its ID
starthub run "your-composition-id" --runner local
```

### With environment variables

```bash
# Set the API base URL (defaults to https://api.starthub.so)
export STARTHUB_API="https://api.starthub.so"

# Set authentication token if needed
export STARTHUB_TOKEN="your-token-here"

# Run with input parameters
starthub run "your-composition-id" --runner local -e username=john -e PORT_2=8080
```

### Example

Given a composition with the following `starthub.json`:

```json
{
  "name": "composition",
  "version": "0.1.0",
  "description": "Saved from editor",
  "inputs": [
    {
      "name": "username",
      "type": "string"
    },
    {
      "name": "PORT_2",
      "type": "string"
    }
  ],
  "outputs": [
    {
      "name": "PORT_1",
      "type": "string"
    },
    {
      "name": "PORT_2",
      "type": "string"
    }
  ],
  "steps": [
    {
      "id": "test_13_2",
      "uses": "ghcr.io/starthubhq/test-13:0.1.0",
      "with": {}
    },
    {
      "id": "test_13_3",
      "uses": "ghcr.io/starthubhq/test-13:0.1.0",
      "with": {}
    }
  ],
  "wires": [
    {
      "from": {
        "source": "inputs",
        "key": "username"
      },
      "to": {
        "step": "test_13_3",
        "input": "PORT_1"
      }
    },
    {
      "from": {
        "source": "inputs",
        "key": "PORT_2"
      },
      "to": {
        "step": "test_13_3",
        "input": "PORT_2"
      }
    },
    {
      "from": {
        "step": "test_13_3",
        "output": "PORT_1"
      },
      "to": {
        "step": "test_13_2",
        "input": "PORT_1"
      }
      },
    {
      "from": {
        "step": "test_13_3",
        "output": "PORT_2"
      },
      "to": {
        "step": "test_13_2",
        "input": "PORT_2"
      }
    }
  ],
  "export": {
    "PORT_1": {
      "from": {
        "step": "test_13_2",
        "output": "PORT_1"
      }
    },
    "PORT_2": {
      "from": {
        "step": "test_13_2",
        "output": "PORT_2"
      }
    }
  }
}
```

You can run it with:

```bash
starthub run "composition-id" --runner local -e username=john -e PORT_2=8080
```

## Implementation Details

### Detection Logic

The CLI uses the `looks_like_composition_id()` function to detect composition IDs:

- Must not contain `{`, `}`, `/`, `:`, `@`
- Must not end with `.json`
- Must not start with `file://`
- Must not be an existing file path
- Must have length > 0

### API Integration

The `fetch_starthub_json()` method in `starthub_api.rs` handles fetching the composition from Supabase storage:

```rust
pub async fn fetch_starthub_json(&self, composition_id: &str) -> Result<String> {
    let url = format!("{}/storage/v1/object/public/compositions/{}/starthub.json", self.base, composition_id);
    
    let mut req = self.http.get(&url);
    req = self.auth_header(req);
    
    let resp = req.send().await?.error_for_status()?;
    let content = resp.text().await?;
    Ok(content)
}
```

### Execution Flow

1. **Dispatch method** in `LocalRunner` checks if the action looks like a composition ID
2. **Fetch composition** from Supabase storage using the API client
3. **Parse JSON** into a `CompositeSpec` struct
4. **Execute composition** using the existing `run_composite()` function
5. **Handle inputs/outputs** through the existing wire system

## Error Handling

The implementation includes proper error handling:

- Network errors when fetching from Supabase storage
- JSON parsing errors for malformed `starthub.json` files
- Missing composition errors (404 responses)
- Authentication errors (401 responses)

## Configuration

### Environment Variables

- `STARTHUB_API`: Base URL for the Starthub API (defaults to `https://api.starthub.so`)
- `STARTHUB_TOKEN`: Authentication token for accessing private compositions

### Storage Path

Compositions are expected to be stored in Supabase storage at:
```
/storage/v1/object/public/compositions/{composition_id}/starthub.json
```

## Testing

You can test the functionality using the provided test script:

```bash
# Edit the script to use a real composition ID
./test_composition.sh
```

## Future Enhancements

Potential improvements could include:

- Caching of fetched compositions
- Support for versioned compositions
- Composition validation before execution
- Support for private compositions with authentication
- Composition metadata and search functionality
