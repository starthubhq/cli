# Atomic Action Format Specification

This document describes the JSON format used for defining actions in the StartHub platform. An action is a reusable module that performs a specific task, implemented as a WASM/Docker module.

## Overview

An action is a declarative specification that defines:
- Input and output interfaces
- Type definitions
- Module metadata
- Repository and licensing information

## Format Structure

### Top-Level Properties

| Property | Type | Required | Description |
|----------|------|----------|-------------|
| `name` | string | Yes | Human-readable name of the action |
| `description` | string | Yes | Description of what the action does |
| `version` | string | Yes | Semantic version of the action |
| `kind` | string | Yes | Type of action (e.g., "wasm", "docker") |
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
  "description": "Description of the input"
}
```

- `name`: Unique identifier for the input/output
- `type`: Type identifier (can be a custom type or built-in type)
- `required`: Whether this input/output is mandatory
- `description`: Human-readable description of the input/output

## Example Action

Here's a complete example of an action specification that demonstrates the format:

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

## Action Functionality

This example action:

1. **Takes inputs**: 
   - `url` (required string): The URL to fetch data from
   - `headers` (optional HttpHeaders): HTTP headers to send with the request

2. **Produces output**: 
   - `body` (HttpResponse): The HTTP response containing status code and response body

3. **Defines types**:
   - `HttpHeaders`: Common HTTP header fields like Content-Type, Authorization, etc.
   - `HttpResponse`: Response structure with status code and body

The action demonstrates a typical HTTP client pattern that can be used in workflows for fetching data from web APIs.

## Type System

The format supports:
- **Built-in types**: `string`, `number`, `boolean`, `Date`, etc.
- **Custom types**: Defined in the `types` section

## Validation Rules

- Input/output names must be unique within their scope
- Type references must be resolvable
- Required inputs must be provided when the action is invoked
- All type definitions must be valid JSON schema
