# Composite Action Format Specification

This document describes the JSON format used for defining composite actions (compositions) in the StartHub platform. A composite action is a workflow that orchestrates multiple steps to process data through a series of transformations.

## Overview

A composite action is a declarative specification that defines:
- Input and output interfaces
- Processing steps (using WASM/Docker modules)
- Data flow between steps
- Type definitions
- Export mappings

## Format Structure

### Top-Level Properties

| Property | Type | Required | Description |
|----------|------|----------|-------------|
| `name` | string | Yes | Human-readable name of the composite action |
| `description` | string | Yes | Description of what the composite action does |
| `version` | string | Yes | Semantic version of the composite action |
| `kind` | string | Yes | Always "composition" for composite actions |
| `manifest_version` | number | Yes | Version of the format specification (currently 1) |
| `repository` | string | No | URL to the source repository |
| `license` | string | No | License identifier (e.g., "MIT") |
| `inputs` | array | Yes | Array of input definitions |
| `outputs` | array | Yes | Array of output definitions |
| `types` | object | No | Custom type definitions |
| `steps` | array | Yes | Array of processing steps |
| `wires` | array | Yes | Array of data flow connections |
| `export` | object | No | Output export mappings |

### Input/Output Definitions

Each input and output has the following structure:

```json
{
  "name": "input-name",
  "type": "type-identifier",
  "required": false,
  "default": "default-value"
}
```

- `name`: Unique identifier for the input/output
- `type`: Type identifier (can be a custom type or built-in type)
- `required`: Whether this input/output is mandatory
- `default`: Default value (optional, for inputs only)

### Steps

Each step represents a processing unit (typically a WASM/Docker module):

```json
{
  "id": "step-identifier",
  "uses": {
    "name": "module-name:version",
    "types": {
      "CustomType": {
        "field1": "string",
        "field2": "number"
      }
    }
  },
  "with": {
    "parameter": "value"
  }
}
```

- `id`: Unique identifier for the step
- `uses.name`: Module name and version to use
- `uses.types`: Type definitions specific to this step
- `with`: Configuration parameters for the step

### Wires (Data Flow)

Wires define how data flows between inputs, steps, and outputs:

```json
{
  "from": {
    "source": "inputs|step-id",
    "key": "input-name|output-name"
  },
  "to": {
    "step": "step-id",
    "input": "input-name"
  }
}
```

- `from.source`: Either "inputs" for composite action inputs or a step ID
- `from.key`: The specific input/output name
- `to.step`: Target step ID
- `to.input`: Target input name

### Export Mappings

Export mappings define how step outputs become composite action outputs:

```json
{
  "output-name": {
    "from": {
      "step": "step-id",
      "output": "output-name"
    }
  }
}
```

## Example Composite Action

Here's a complete example of a composite action specification that demonstrates the format:

```json
{
  "name": "an-example-composition",
  "description": "Saved from editor",
  "version": "0.0.2",
  "kind": "composition",
  "manifest_version": 1,
  "repository": "",
  "license": "MIT",
  "inputs": [
    {
      "name": "some-input",
      "type": "string",
      "required": false
    },
    {
      "name": "PORT_1",
      "type": "starthubhq/http-get-wasm:0.0.16/HttpHeaders",
      "required": false
    },
    {
      "name": "PORT_2",
      "type": "User",
      "default": "User",
      "required": false
    }
  ],
  "outputs": [
    {
      "name": "PORT_1",
      "type": "User",
      "required": false
    }
  ],
  "types": {
    "User": {
      "id": "string",
      "name": "string",
      "email": "string",
      "createdAt": "Date"
    }
  },
  "steps": [
    {
      "id": "http_get_wasm",
      "uses": {
        "name": "starthubhq/http-get-wasm:0.0.16",
        "types": {
          "HttpHeaders": {
            "Accept": "string",
            "X-API-Key": "string",
            "User-Agent": "string",
            "Content-Type": "string",
            "Authorization": "string"
          },
          "HttpResponse": {
            "body": "string",
            "status": "number"
          }
        }
      },
      "with": {}
    },
    {
      "id": "stringify_wasm",
      "uses": {
        "name": "stringify-wasm:0.0.5",
        "types": {}
      },
      "with": {}
    },
    {
      "id": "parse_wasm",
      "uses": {
        "name": "parse-wasm:0.0.10",
        "types": {}
      },
      "with": {}
    }
  ],
  "wires": [
    {
      "from": {
        "source": "inputs",
        "key": "some-input"
      },
      "to": {
        "step": "http_get_wasm",
        "input": "url"
      }
    },
    {
      "from": {
        "source": "inputs",
        "key": "PORT_1"
      },
      "to": {
        "step": "http_get_wasm",
        "input": "headers"
      }
    },
    {
      "from": {
        "step": "http_get_wasm",
        "output": "body"
      },
      "to": {
        "step": "stringify_wasm",
        "input": "object"
      }
    },
    {
      "from": {
        "source": "inputs",
        "key": "PORT_2"
      },
      "to": {
        "step": "parse_wasm",
        "input": "type"
      }
    },
    {
      "from": {
        "step": "stringify_wasm",
        "output": "string"
      },
      "to": {
        "step": "parse_wasm",
        "input": "string"
      }
    }
  ],
  "export": {
    "PORT_1": {
      "from": {
        "step": "parse_wasm",
        "output": "response"
      }
    }
  }
}
```

## Data Flow in the Example

This example composite action:

1. **Takes inputs**: `some-input` (URL), `PORT_1` (HTTP headers), and `PORT_2` (type specification)
2. **Makes HTTP request**: Uses `http-get-wasm` to fetch data from the URL with headers
3. **Stringifies response**: Converts the HTTP response body to a string using `stringify-wasm`
4. **Parses data**: Uses `parse-wasm` to parse the stringified data according to the specified type
5. **Exports result**: Returns the parsed response as `PORT_1`

The composite action demonstrates a common pattern: HTTP request → stringify → parse → export, which is useful for API data processing workflows.

## Type System

The format supports:
- **Built-in types**: `string`, `number`, `boolean`, `Date`, etc.
- **Custom types**: Defined in the `types` section
- **Step-specific types**: Defined in each step's `uses.types`

## Validation Rules

- All step IDs must be unique
- All wire connections must reference valid sources and targets
- Input/output names must be unique within their scope
- Export mappings must reference valid step outputs
- Type references must be resolvable
