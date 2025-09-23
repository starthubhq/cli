# Composition Format Specification

This document describes the JSON format used for defining compositions in the StartHub platform. A composition is a workflow that orchestrates multiple steps to process data through a series of transformations.

## Overview

A composition is a declarative specification that defines:
- Input and output interfaces
- Processing steps (using WASM modules)
- Data flow between steps
- Type definitions
- Export mappings

## Format Structure

### Top-Level Properties

| Property | Type | Required | Description |
|----------|------|----------|-------------|
| `name` | string | Yes | Human-readable name of the composition |
| `description` | string | Yes | Description of what the composition does |
| `version` | string | Yes | Semantic version of the composition |
| `kind` | string | Yes | Always "composition" for compositions |
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

Each step represents a processing unit (a WASM/Docker module):

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

- `from.source`: Either "inputs" for composition inputs or a step ID
- `from.key`: The specific input/output name
- `to.step`: Target step ID
- `to.input`: Target input name

### Export Mappings

Export mappings define how step outputs become composition outputs:

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

## Example WASM Module

Here's a complete example of a WASM module specification that demonstrates the format:

```json
{
  "name": "http-get-wasm",
  "description": "HTTP GET request module for fetching data from web APIs",
  "version": "0.0.12",
  "kind": "wasm",
  "manifest_version": 1,
  "repository": "github.com/starthubhq/http-get-wasm",
  "license": "MIT",
  "inputs": [
    {
      "name": "url",
      "type": "string",
      "required": true,
      "description": "The URL to fetch data from"
    },
    {
      "name": "headers",
      "type": "HttpHeaders",
      "required": false,
      "description": "Optional HTTP headers to send with the request"
    }
  ],
  "outputs": [
    {
      "name": "body",
      "type": "HttpResponse",
      "required": true,
      "description": "Response"
    }
  ],
  "types": {
    "HttpHeaders": {
      "Content-Type": "string",
      "Authorization": "string",
      "User-Agent": "string",
      "Accept": "string",
      "X-API-Key": "string"
    },
    "HttpResponse": {
      "status": "number",
      "body": "string"
    }
  }
}
```

## Module Functionality

This example WASM module:

1. **Takes inputs**: 
   - `url` (required string): The URL to fetch data from
   - `headers` (optional HttpHeaders): HTTP headers to send with the request

2. **Produces output**: 
   - `body` (HttpResponse): The HTTP response containing status code and response body

3. **Defines types**:
   - `HttpHeaders`: Common HTTP header fields like Content-Type, Authorization, etc.
   - `HttpResponse`: Response structure with status code and body

The module demonstrates a typical HTTP client pattern that can be used in compositions for fetching data from web APIs.

## Type System

The format supports:
- **Built-in types**: `string`, `number`, `boolean`, `Date`, etc.
- **Custom types**: Defined in the `types` section
- **Generic types**: Using angle bracket notation like `type<t>`
- **Step-specific types**: Defined in each step's `uses.types`

## Validation Rules

- All step IDs must be unique
- All wire connections must reference valid sources and targets
- Input/output names must be unique within their scope
- Export mappings must reference valid step outputs
- Type references must be resolvable
