---
sidebar_position: 4
---

# Lock file

The `starthub-lock.json` file defines the metadata and schema for a Starthub action. It serves as a contract that describes what inputs the action accepts, what outputs it produces, and what permissions it requires.

## Structure

The lock file is a JSON document with the following structure:

```json
{
  "name": "string",
  "description": "string",
  "version": "string",
  "kind": "string",
  "manifest_version": number,
  "repository": "string",
  "license": "string",
  "inputs": [],
  "outputs": [],
  "permissions": {},
  "mirrors": []
}
```

## Field Descriptions

### Top-level Fields

- **`name`** (string, required): Unique identifier for the action, typically matching the directory name
- **`description`** (string, required): Human-readable description of what the action does
- **`version`** (string, required): Semantic version following MAJOR.MINOR.PATCH format (e.g., "0.0.1")
- **`kind`** (string, required): The type of action runtime. Currently supported values:
  - `"docker"` - Containerized actions that run in Docker
  - `"wasm"` - WebAssembly-based actions
- **`manifest_version`** (number, required): Version of the lock file schema format itself. This allows the schema to evolve while maintaining backward compatibility
- **`repository`** (string, required): Source repository location (e.g., "github.com/user/repo")
- **`license`** (string, required): SPDX license identifier (e.g., "MIT", "Apache-2.0")

### Inputs

The `inputs` array defines all input parameters the action accepts. Each input object has the following structure:

```json
{
  "name": "string",          // Parameter name (must be unique)
  "description": "string",  // Human-readable description
  "type": "string",          // Parameter type: "string", "boolean", "string[]", "number", etc.
  "required": boolean,       // Whether the parameter is required
  "default": any            // Default value (can be null, string, boolean, number, or array)
}
```

**Example:**
```json
{
  "name": "api_token",
  "description": "Digital Ocean API token",
  "type": "string",
  "required": true,
  "default": null
}
```

**Supported Types:**
- `"string"` - Text values
- `"boolean"` - True/false values
- `"string[]"` - Array of strings
- `"number"` - Numeric values

### Outputs

The `outputs` array defines all outputs the action produces. Each output object has the following structure:

```json
{
  "name": "string",          // Output name (must be unique)
  "description": "string",  // Human-readable description
  "type": "string",          // Output type
  "required": boolean        // Whether the output is required
}
```

**Example:**
```json
{
  "name": "statefile",
  "description": "OpenTofu state file in JSON format",
  "type": "string",
  "required": true
}
```

### Permissions

The `permissions` object defines runtime security permissions for the action:

```json
{
  "net": ["string"]  // Network permissions (e.g., ["http", "https"])
}
```

- **`net`** (array of strings): Specifies which network protocols the action is allowed to use. Common values:
  - `"http"` - HTTP protocol access
  - `"https"` - HTTPS protocol access
  - `"tcp"` - TCP protocol access
  - `"udp"` - UDP protocol access

**Example:**
```json
{
  "permissions": {
    "net": ["http", "https"]
  }
}
```

### Mirrors

The `mirrors` array is used for mirror/repository configurations. This is typically an empty array for most actions:

```json
{
  "mirrors": []
}
```

## Complete Example

Here's a complete example of a lock file for a Digital Ocean droplet creation action:

```json
{
  "name": "do-create-droplet-tf",
  "description": "Create a new Digital Ocean droplet using OpenTofu",
  "version": "0.0.1",
  "kind": "docker",
  "manifest_version": 1,
  "repository": "github.com/tgirotto/do-create-droplet-tf",
  "license": "MIT",
  "inputs": [
    {
      "name": "api_token",
      "description": "Digital Ocean API token",
      "type": "string",
      "required": true,
      "default": null
    },
    {
      "name": "name",
      "description": "Name of the droplet",
      "type": "string",
      "required": true,
      "default": null
    },
    {
      "name": "region",
      "description": "Region where the droplet will be created (e.g., 'nyc1', 'nyc3', 'sfo3')",
      "type": "string",
      "required": true,
      "default": null
    },
    {
      "name": "backups",
      "description": "Enable backups for the droplet",
      "type": "boolean",
      "required": false,
      "default": false
    },
    {
      "name": "tags",
      "description": "List of tags to apply to the droplet",
      "type": "string[]",
      "required": false,
      "default": null
    }
  ],
  "outputs": [
    {
      "name": "statefile",
      "description": "OpenTofu state file in JSON format",
      "type": "string",
      "required": true
    }
  ],
  "permissions": {
    "net": ["http", "https"]
  },
  "mirrors": []
}
```

## Best Practices

1. **Versioning**: Use semantic versioning for the `version` field. Increment:
   - MAJOR version for breaking changes
   - MINOR version for new features (backward compatible)
   - PATCH version for bug fixes

2. **Descriptions**: Write clear, concise descriptions for all inputs and outputs. This helps users understand what each parameter does.

3. **Required vs Optional**: Mark inputs as `required: true` only when they are essential for the action to function. Use `required: false` with appropriate defaults when possible.

4. **Permissions**: Follow the principle of least privilege. Only request the minimum network permissions needed for the action to function.

5. **Type Safety**: Use the most specific type possible. For example, use `"string[]"` for arrays of strings rather than a generic type.

