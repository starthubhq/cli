# Type Checking

This document describes how type checking is implemented in StartHub compositions, ensuring type safety both during development and at runtime.

## Overview

Type checking in compositions is performed at two critical points:
- **Save time**: During development when saving composition files
- **Runtime**: When executing compositions to ensure data integrity

Both phases use TypeScript notation and type system principles to provide comprehensive type safety.

## Save Time Type Checking

When you save a composition file, the system performs static type analysis to catch errors before execution:

### Input/Output Type Validation

```json
{
  "inputs": [
    {
      "name": "url",
      "type": "string",
      "required": true
    },
    {
      "name": "headers",
      "type": "starthubhq/http-get-wasm:0.0.16/HttpHeaders",
      "required": false
    }
  ],
  "outputs": [
    {
      "name": "response",
      "type": "starthubhq/http-get-wasm:0.0.16/HttpResponse",
      "required": true
    }
  ]
}
```

The system validates:
- Type references are resolvable
- Required inputs are properly defined
- Output types match step outputs
- Custom types are properly structured

### Step Type Validation

```json
{
  "steps": [
    {
      "id": "http_get_wasm",
      "uses": {
        "name": "starthubhq/http-get-wasm:0.0.16",
        "types": {
          "HttpHeaders": {
            "Content-Type": "string",
            "Authorization": "string"
          }
        }
      }
    }
  ]
}
```

The system checks:
- Step module references are valid
- Input/output type compatibility
- Type definitions match module specifications
- Generic type constraints are satisfied

### Wire Type Validation

```json
{
  "wires": [
    {
      "from": {
        "source": "inputs",
        "key": "url"
      },
      "to": {
        "step": "http_get_wasm",
        "input": "url"
      }
    }
  ]
}
```

The system validates:
- Source and target types are compatible
- Wire connections reference valid inputs/outputs
- Type coercion rules are followed
- Generic type parameters are properly resolved

## Runtime Type Checking

During composition execution, runtime type checking ensures data integrity:

### Input Validation

Before step execution, inputs are validated against their declared types:

```typescript
// Runtime type checking example
interface HttpHeaders {
  "Content-Type"?: string;
  "Authorization"?: string;
  "User-Agent"?: string;
}

function validateInput(input: any, expectedType: string): boolean {
  switch (expectedType) {
    case "string":
      return typeof input === "string";
    case "starthubhq/http-get-wasm:0.0.16/HttpHeaders":
      return validateHttpHeaders(input);
    default:
      return validateCustomType(input, expectedType);
  }
}
```

### Step Output Validation

After each step execution, outputs are validated:

```typescript
interface HttpResponse {
  status: number;
  body: string;
}

function validateStepOutput(output: any, expectedType: string): boolean {
  if (expectedType === "starthubhq/http-get-wasm:0.0.16/HttpResponse") {
    return output && 
           typeof output.status === "number" && 
           typeof output.body === "string";
  }
  return validateCustomType(output, expectedType);
}
```

### Wire Type Coercion

When data flows between steps, type coercion is applied:

```typescript
function coerceType(value: any, fromType: string, toType: string): any {
  // Handle common type coercions
  if (fromType === "string" && toType === "number") {
    return parseFloat(value);
  }
  
  if (fromType === "object" && toType === "string") {
    return JSON.stringify(value);
  }
  
  // Handle custom type conversions
  return convertCustomType(value, fromType, toType);
}
```

## TypeScript Integration

The type system is built on TypeScript principles:

### Generic Types

```json
{
  "inputs": [
    {
      "name": "data",
      "type": "type<T>",
      "default": "User"
    }
  ],
  "types": {
    "User": {
      "id": "string",
      "name": "string"
    }
  }
}
```

Generic types are resolved at runtime based on the default value or explicit type parameters.

### Union Types

```json
{
  "types": {
    "ApiResponse": {
      "success": "boolean",
      "data": "User | Error",
      "status": "number"
    }
  }
}
```

Union types allow for flexible data structures that can represent multiple possible states.

### Optional Properties

```json
{
  "types": {
    "HttpHeaders": {
      "Content-Type": "string?",
      "Authorization": "string?",
      "User-Agent": "string?"
    }
  }
}
```

Optional properties (marked with `?`) allow for flexible input structures.

## Error Handling

Type checking errors are reported with detailed information:

### Save Time Errors

```
Error: Type mismatch in wire from "inputs.url" to "http_get_wasm.url"
Expected: string
Actual: number
Location: wires[0]
```

### Runtime Errors

```
Runtime Error: Step "http_get_wasm" output validation failed
Expected: starthubhq/http-get-wasm:0.0.16/HttpResponse
Actual: { status: "200", body: 123 }
Error: Property "body" should be string, got number
```

## Best Practices

1. **Use specific types**: Prefer concrete types over generic ones when possible
2. **Validate early**: Catch type errors at save time rather than runtime
3. **Document types**: Provide clear descriptions for custom types
4. **Test edge cases**: Verify type coercion behavior with various inputs
5. **Use TypeScript tooling**: Leverage IDE support for type checking during development

## Type System Features

- **Static analysis**: Catch errors before execution
- **Runtime validation**: Ensure data integrity during execution
- **Type coercion**: Automatic conversion between compatible types
- **Generic support**: Flexible type parameters
- **Union types**: Multiple possible types for a single value
- **Optional properties**: Flexible object structures
- **Custom types**: User-defined type definitions
- **Module types**: Step-specific type definitions
