# Input/Output Definition and Type Boundaries

This document explains how actions define their interfaces and interact with other actions through well-defined type boundaries.

## Core Principle: Action Interface Isolation

Every action can only interface with other actions via its own types. This creates clear boundaries and prevents type leakage between actions.

## Action Interface Design

### Atomic Actions

Atomic actions (WASM/Docker modules) define their complete interface:

```json
{
  "name": "http-get-wasm",
  "inputs": [
    {
      "name": "url",
      "type": "string",
      "required": true
    },
    {
      "name": "headers", 
      "type": "HttpHeaders",
      "required": false
    }
  ],
  "outputs": [
    {
      "name": "body",
      "type": "HttpResponse",
      "required": true
    }
  ],
  "types": {
    "HttpHeaders": {
      "Content-Type": "string",
      "Authorization": "string"
    },
    "HttpResponse": {
      "status": "number",
      "body": "string"
    }
  }
}
```

The action defines:
- **Input types**: What data it accepts
- **Output types**: What data it produces  
- **Internal types**: Types it uses internally (HttpHeaders, HttpResponse)

### Composite Actions

Composite actions define their own interface, independent of their internal implementation:

```json
{
  "name": "api-data-processor",
  "inputs": [
    {
      "name": "api_url",
      "type": "string",
      "required": true
    },
    {
      "name": "auth_token",
      "type": "string", 
      "required": false
    }
  ],
  "outputs": [
    {
      "name": "processed_data",
      "type": "ProcessedData",
      "required": true
    }
  ],
  "types": {
    "ProcessedData": {
      "id": "string",
      "content": "string",
      "timestamp": "Date"
    }
  }
}
```

Notice that the composite action:
- Defines its own input/output types
- Doesn't expose the types from its internal steps
- Creates a clean abstraction over the implementation

## Type Boundary Rules

### 1. Interface Isolation

Actions can only use their own types in their input/output definitions:

```json
// ✅ CORRECT: Using own types
{
  "inputs": [
    {
      "name": "data",
      "type": "MyCustomType"  // Defined in this action's types
    }
  ]
}

// ❌ INCORRECT: Using another action's types
{
  "inputs": [
    {
      "name": "data", 
      "type": "other-action/SomeType"  // Not allowed
    }
  ]
}
```

### 2. Internal Type Usage

Actions can use types from their dependencies internally (in wires), but not in their interface:

```json
{
  "steps": [
    {
      "id": "http_step",
      "uses": {
        "name": "http-get-wasm:0.0.16",
        "types": {
          "HttpHeaders": {
            "Authorization": "string"
          }
        }
      }
    }
  ],
  "wires": [
    {
      "from": {
        "source": "inputs",
        "key": "auth_token"
      },
      "to": {
        "step": "http_step",
        "input": "headers"
      }
    }
  ]
}
```

The wire can connect the action's own input (`auth_token`) to the step's input (`headers`), but the action's interface remains clean.

### 3. Type Coercion at Boundaries

When data flows between actions, type coercion happens at the interface boundary:

```typescript
// Action A outputs: { result: string }
// Action B expects: { data: string }

// Type coercion maps result -> data
function coerceAtBoundary(output: any, expectedInput: any): any {
  return {
    data: output.result  // Map result field to data field
  };
}
```

## Benefits of This Approach

### 1. Clear Abstractions

Each action presents a clean, stable interface:

```json
// Clean interface - no implementation details leaked
{
  "inputs": [
    { "name": "query", "type": "string" },
    { "name": "limit", "type": "number" }
  ],
  "outputs": [
    { "name": "results", "type": "SearchResults" }
  ]
}
```

### 2. Implementation Flexibility

Actions can change their internal implementation without affecting their interface:

```json
// Version 1: Uses http-get-wasm internally
{
  "steps": [
    { "id": "fetch", "uses": "http-get-wasm:0.0.16" }
  ]
}

// Version 2: Uses fetch-wasm internally  
{
  "steps": [
    { "id": "fetch", "uses": "fetch-wasm:0.0.8" }
  ]
}

// Interface stays the same!
```

### 3. Better Testing

You can test actions through their interface without knowing internal details:

```typescript
// Test the action's interface
const result = await executeAction("api-processor", {
  api_url: "https://api.example.com",
  auth_token: "abc123"
});

// Don't need to know it uses http-get-wasm internally
expect(result.processed_data).toBeDefined();
```

### 4. Composition Clarity

When composing actions, you only need to understand their interfaces:

```json
{
  "steps": [
    {
      "id": "data_fetcher",
      "uses": "api-processor:1.0.0"  // Only need to know its interface
    },
    {
      "id": "data_transformer", 
      "uses": "data-transform:2.0.0"  // Only need to know its interface
    }
  ]
}
```

## Type Resolution

### Action-Level Types

Types are resolved within each action's scope:

```json
{
  "types": {
    "User": {
      "id": "string",
      "name": "string"
    }
  },
  "inputs": [
    {
      "name": "user_data",
      "type": "User"  // Resolves to the User type above
    }
  ]
}
```

### Step-Level Types

Steps can define their own types for internal use:

```json
{
  "steps": [
    {
      "id": "http_step",
      "uses": {
        "name": "http-get-wasm:0.0.16",
        "types": {
          "HttpHeaders": {
            "Content-Type": "string"
          }
        }
      }
    }
  ]
}
```

## Best Practices

1. **Define clear interfaces**: Each action should have a well-defined input/output interface
2. **Use descriptive type names**: Make it clear what each type represents
3. **Keep interfaces stable**: Don't change input/output types unless necessary
4. **Document type purposes**: Provide clear descriptions for complex types
5. **Test interface boundaries**: Ensure type coercion works correctly
6. **Minimize interface complexity**: Keep input/output types simple and focused

## Error Handling

Type boundary violations are caught at save time:

```
Error: Action "api-processor" cannot use type "http-get-wasm/HttpHeaders" 
in its interface. Actions can only use their own types in inputs/outputs.

Location: inputs[0].type
```

This ensures that action interfaces remain clean and self-contained.
