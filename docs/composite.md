# Composite Action Format Specification

This document describes the JSON format used for defining composite actions (compositions) in the StartHub platform. A composite action is a workflow that orchestrates multiple steps to process data through a series of transformations.

## Overview

A composite action is a declarative specification that defines:
- Input and output interfaces
- Processing steps (using WASM/Docker modules)
- Data flow between steps using template interpolation
- Type definitions
- Output transformations

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
| `outputs` | array | Yes | Array of output definitions with transformations |
| `types` | object | No | Custom type definitions |
| `steps` | array | Yes | Array of processing steps |

### Input Definitions

Each input has the following structure:

```json
{
  "name": "input-name",
  "description": "Description of the input",
  "type": "type-identifier",
  "required": true,
  "default": null
}
```

- `name`: Unique identifier for the input
- `description`: Human-readable description
- `type`: Type identifier (can be a custom type or built-in type)
- `required`: Whether this input is mandatory
- `default`: Default value (optional)

### Steps

Each step represents a processing unit (typically a WASM/Docker module):

```json
{
  "id": "step-identifier",
  "uses": "module-name:version",
  "types": {
    "CustomType": {
      "field1": "string",
      "field2": "number"
    }
  },
  "inputs": [
    {
      "type": "InputType",
      "value": "{{template.interpolation}}"
    }
  ],
  "outputs": [
    "OutputType"
  ]
}
```

- `id`: Unique identifier for the step
- `uses`: Module name and version to use
- `types`: Type definitions specific to this step
- `inputs`: Array of input values with types and template interpolation
- `outputs`: Array of output type names

### Template Interpolation

The format supports template interpolation using `{{}}` syntax:

- `{{inputs.input_name}}`: Reference to composite action inputs
- `{{step_id.output_field}}`: Reference to step outputs
- `{{step_id.body[0].field}}`: Access to nested JSON properties

### Output Transformations

Outputs can transform and restructure data using template interpolation:

```json
{
  "name": "{{inputs.location_name}}",
  "type": "GeocodingResponse",
  "value": {
    "lat": "{{get_geocoding_response.body[0].lat}}",
    "lon": "{{get_geocoding_response.body[0].lon}}",
    "country": "{{get_geocoding_response.body[0].country}}"
  }
}
```

- `name`: Can use template interpolation for dynamic naming
- `type`: The type of the output data
- `value`: Object structure with template interpolation for data transformation

## Example Composite Action

Here's a complete example of a composite action that fetches coordinates from the OpenWeather geocoding API:

```json
{
  "name": "coordinates-by-location-name",
  "description": "Get coordinates by location name",
  "version": "0.0.1",
  "kind": "composition",
  "manifest_version": 1,
  "repository": "github.com/tgirotto/coordinates-by-location-name",
  "license": "MIT",
  "inputs": [
    {
      "name": "location_name",
      "description": "The name of the location to get weather for",
      "type": "string",
      "required": true,
      "default": null
    },
    {
      "name": "open_weather_api_key",
      "description": "OpenWeatherMap API key",
      "type": "string",
      "required": true,
      "default": null
    }
  ],
  "steps": [
    {
      "id": "get_geocoding_response",
      "uses": "starthubhq/http-get-wasm:0.0.16",
      "types": {
        "HttpHeaders": {
          "Content-Type": "string",
          "Authorization": "string"
        },
        "HttpResponse": {
          "status": "number",
          "body": "string"
        }
      },
      "inputs": [
        {
          "type": "HttpHeaders",
          "value": {
            "Content-Type": "application/json",
            "Authorization": "Bearer {{inputs.open_weather_api_key}}"
          }
        },
        {
          "type": "string",
          "value": "https://api.openweathermap.org/geo/1.0/direct?q={{inputs.location_name}}&limit=1&appid={{inputs.open_weather_api_key}}"
        }
      ],
      "outputs": [
        "HttpResponse"
      ]
    }
  ],
  "outputs": [
    {
      "name": "{{inputs.location_name}}",
      "type": "GeocodingResponse",
      "value": {
        "local_names": {
          "en": "{{get_geocoding_response.body[0].local_names.en}}",
          "it": "{{get_geocoding_response.body[0].local_names.it}}",
          "fr": "{{get_geocoding_response.body[0].local_names.fr}}",
          "de": "{{get_geocoding_response.body[0].local_names.de}}",
          "es": "{{get_geocoding_response.body[0].local_names.es}}",
          "pt": "{{get_geocoding_response.body[0].local_names.pt}}",
          "ru": "{{get_geocoding_response.body[0].local_names.ru}}",
          "zh": "{{get_geocoding_response.body[0].local_names.zh}}",
          "ja": "{{get_geocoding_response.body[0].local_names.ja}}",
          "ko": "{{get_geocoding_response.body[0].local_names.ko}}",
          "ar": "{{get_geocoding_response.body[0].local_names.ar}}",
          "hi": "{{get_geocoding_response.body[0].local_names.hi}}"
        },
        "lat": "{{get_geocoding_response.body[0].lat}}",
        "lon": "{{get_geocoding_response.body[0].lon}}",
        "country": "{{get_geocoding_response.body[0].country}}",
        "state": "{{get_geocoding_response.body[0].state}}"
      }
    }
  ],
  "types": {
    "GeocodingResponse": [
      {
        "name": "string",
        "local_names": {
          "en": "string",
          "it": "string",
          "fr": "string",
          "de": "string",
          "es": "string",
          "pt": "string",
          "ru": "string",
          "zh": "string",
          "ja": "string",
          "ko": "string",
          "ar": "string",
          "hi": "string"
        },
        "lat": "number",
        "lon": "number",
        "country": "string",
        "state": "string"
      }
    ]
  }
}
```

## Data Flow in the Example

This example composite action:

1. **Takes inputs**: `location_name` (string) and `open_weather_api_key` (string)
2. **Makes HTTP request**: Uses `http-get-wasm` to fetch geocoding data from OpenWeather API
3. **Transforms output**: Converts the raw HTTP response into structured `GeocodingResponse` data
4. **Returns structured data**: Provides coordinates, local names, and location metadata

The composite action demonstrates the power of template interpolation for data transformation, converting raw HTTP responses into clean, typed data structures.

## Type System

The format supports:
- **Built-in types**: `string`, `number`, `boolean`, `Date`, etc.
- **Custom types**: Defined in the `types` section
- **Step-specific types**: Defined in each step's `types` section
- **Template interpolation**: Automatic JSON parsing for nested property access

## Template Interpolation Features

- **Input references**: `{{inputs.input_name}}`
- **Step output references**: `{{step_id.output_field}}`
- **Nested property access**: `{{step_id.body[0].field}}`
- **Dynamic naming**: Output names can use template interpolation
- **Type safety**: Template interpolation is validated against type definitions

## Validation Rules

- All step IDs must be unique
- Template interpolation must reference valid sources
- Input/output names must be unique within their scope
- Type references must be resolvable
- Template interpolation must produce compatible types
- Step inputs must match expected types
- Output transformations must produce valid types

## Benefits of This Format

1. **Readability**: Data flow is clear and co-located
2. **Type Safety**: Complete type checking for all operations
3. **Flexibility**: Rich template interpolation for data transformation
4. **Maintainability**: No scattered wire definitions
5. **Developer Experience**: Intuitive syntax with full IDE support