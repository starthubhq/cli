# WASM vs Docker Modules: When to Use Each

This document explains when to use WASM modules versus Docker modules in StartHub actions, with practical examples and use cases.

## Overview

StartHub supports two types of atomic actions:
- **WASM modules**: Lightweight, fast, sandboxed execution
- **Docker modules**: Full containerized applications with system access

## WASM Modules

### When to Use WASM

Use WASM modules when you need:
- **Fast execution** - Near-native performance
- **Lightweight deployment** - Small binary sizes
- **Sandboxed security** - No system access
- **Cross-platform compatibility** - Runs anywhere
- **Simple data processing** - Pure computation tasks

### WASM Use Cases

#### 1. Data Transformation
```json
{
  "name": "json-parser",
  "kind": "wasm",
  "inputs": [
    { "name": "json_string", "type": "string" }
  ],
  "outputs": [
    { "name": "parsed_object", "type": "object" }
  ]
}
```
**Why WASM**: Pure computation, no I/O, fast parsing

#### 2. Mathematical Operations
```json
{
  "name": "statistical-calculator",
  "kind": "wasm", 
  "inputs": [
    { "name": "data_points", "type": "number[]" }
  ],
  "outputs": [
    { "name": "statistics", "type": "Statistics" }
  ]
}
```
**Why WASM**: CPU-intensive calculations, no external dependencies

#### 3. Text Processing
```json
{
  "name": "text-analyzer",
  "kind": "wasm",
  "inputs": [
    { "name": "text", "type": "string" },
    { "name": "language", "type": "string" }
  ],
  "outputs": [
    { "name": "analysis", "type": "TextAnalysis" }
  ]
}
```
**Why WASM**: Pure text processing, no network calls

#### 4. Data Validation
```json
{
  "name": "schema-validator",
  "kind": "wasm",
  "inputs": [
    { "name": "data", "type": "object" },
    { "name": "schema", "type": "object" }
  ],
  "outputs": [
    { "name": "is_valid", "type": "boolean" },
    { "name": "errors", "type": "string[]" }
  ]
}
```
**Why WASM**: Fast validation, no external resources needed

#### 5. Simple API Calls
```json
{
  "name": "weather-fetcher",
  "kind": "wasm",
  "inputs": [
    { "name": "city", "type": "string" },
    { "name": "api_key", "type": "string" }
  ],
  "outputs": [
    { "name": "weather_data", "type": "WeatherData" }
  ]
}
```
**Why WASM**: Simple HTTP requests, lightweight, fast execution

### WASM Advantages

- **Performance**: Near-native execution speed
- **Size**: Small binary footprints (KB to MB)
- **Security**: Sandboxed execution environment
- **Portability**: Runs on any platform
- **Startup time**: Instant execution
- **Resource usage**: Minimal memory and CPU

### WASM Limitations

- **No system access**: Can't access files or OS (but can make HTTP requests)
- **Limited libraries**: Only what's compiled into the WASM
- **Language constraints**: Must compile to WASM
- **No persistent state**: Stateless execution only
- **Simple HTTP only**: Basic API calls, no complex networking

## Docker Modules

### When to Use Docker

Use Docker modules when you need:
- **System access** - File system, network, OS APIs
- **Complex dependencies** - Large libraries or frameworks
- **Existing applications** - Wrap existing tools/services
- **Language flexibility** - Use any programming language
- **Persistent state** - Database connections, caches

### Docker Use Cases

#### 1. Database Operations
```json
{
  "name": "postgres-query",
  "kind": "docker",
  "inputs": [
    { "name": "query", "type": "string" },
    { "name": "connection_string", "type": "string" }
  ],
  "outputs": [
    { "name": "results", "type": "object[]" }
  ]
}
```
**Why Docker**: Needs database drivers, network access, persistent connections

#### 2. File Processing
```json
{
  "name": "image-processor",
  "kind": "docker",
  "inputs": [
    { "name": "image_data", "type": "string" },
    { "name": "operations", "type": "ImageOps[]" }
  ],
  "outputs": [
    { "name": "processed_image", "type": "string" }
  ]
}
```
**Why Docker**: Needs image libraries (ImageMagick, OpenCV), file system access

#### 3. Machine Learning
```json
{
  "name": "ml-predictor",
  "kind": "docker",
  "inputs": [
    { "name": "model_path", "type": "string" },
    { "name": "input_data", "type": "object" }
  ],
  "outputs": [
    { "name": "prediction", "type": "number" }
  ]
}
```
**Why Docker**: Large ML frameworks (TensorFlow, PyTorch), GPU access, model files

#### 4. Complex API Integration
```json
{
  "name": "slack-notifier",
  "kind": "docker",
  "inputs": [
    { "name": "message", "type": "string" },
    { "name": "webhook_url", "type": "string" }
  ],
  "outputs": [
    { "name": "success", "type": "boolean" }
  ]
}
```
**Why Docker**: Complex HTTP libraries, authentication, retry logic, rate limiting

#### 5. Data Pipeline
```json
{
  "name": "etl-processor",
  "kind": "docker",
  "inputs": [
    { "name": "source_config", "type": "object" },
    { "name": "destination_config", "type": "object" }
  ],
  "outputs": [
    { "name": "processed_count", "type": "number" }
  ]
}
```
**Why Docker**: Complex ETL libraries, database connections, file system access

### Docker Advantages

- **Full system access**: Files, network, OS APIs
- **Language flexibility**: Any programming language
- **Rich ecosystems**: Access to entire package ecosystems
- **Existing tools**: Wrap existing applications
- **Complex dependencies**: Large frameworks and libraries
- **Persistent state**: Database connections, caches

### Docker Limitations

- **Size**: Large images (MB to GB)
- **Startup time**: Container initialization overhead
- **Resource usage**: Higher memory and CPU requirements
- **Security**: Requires careful container security
- **Platform dependencies**: May need specific OS features

## Decision Matrix

| Requirement | WASM | Docker |
|-------------|------|--------|
| **Performance** | ✅ Excellent | ⚠️ Good |
| **Size** | ✅ Small | ❌ Large |
| **Security** | ✅ Sandboxed | ⚠️ Containerized |
| **System Access** | ❌ None | ✅ Full |
| **Language Support** | ⚠️ Limited | ✅ Any |
| **Dependencies** | ⚠️ Compiled in | ✅ Any |
| **Startup Time** | ✅ Instant | ❌ Slow |
| **Resource Usage** | ✅ Minimal | ❌ High |

## Real-World Examples

### Use WASM For:

**Data Processing Pipeline**
```json
{
  "name": "data-cleaner",
  "kind": "wasm",
  "description": "Cleans and validates data using pure algorithms"
}
```

**Mathematical Calculations**
```json
{
  "name": "financial-calculator", 
  "kind": "wasm",
  "description": "Performs complex financial calculations"
}
```

**Text Analysis**
```json
{
  "name": "sentiment-analyzer",
  "kind": "wasm", 
  "description": "Analyzes text sentiment using NLP algorithms"
}
```

**Simple API Calls**
```json
{
  "name": "currency-converter",
  "kind": "wasm",
  "description": "Fetches exchange rates from a simple API"
}
```

### Use Docker For:

**Database Operations**
```json
{
  "name": "mysql-migrator",
  "kind": "docker",
  "description": "Migrates data between MySQL databases"
}
```

**File Processing**
```json
{
  "name": "pdf-generator",
  "kind": "docker",
  "description": "Generates PDFs from HTML templates"
}
```

**Complex API Integration**
```json
{
  "name": "stripe-payment",
  "kind": "docker", 
  "description": "Processes payments via Stripe API with retry logic and webhooks"
}
```

## Performance Considerations

### WASM Performance
- **Startup**: < 1ms
- **Memory**: 1-10MB
- **CPU**: Near-native speed
- **Size**: 10KB-10MB

### Docker Performance  
- **Startup**: 100-1000ms
- **Memory**: 50-500MB
- **CPU**: Good (with overhead)
- **Size**: 50MB-5GB

## Best Practices

### Choose WASM When:
1. **Pure computation** - No I/O operations needed
2. **Performance critical** - Need maximum speed
3. **Simple logic** - Basic data processing
4. **Resource constrained** - Limited memory/CPU
5. **Security sensitive** - Need sandboxed execution

### Choose Docker When:
1. **System integration** - Need file/network access
2. **Complex dependencies** - Large frameworks required
3. **Existing tools** - Wrapping existing applications
4. **Language requirements** - Need specific language features
5. **Stateful operations** - Database connections, caches

## Migration Path

You can start with WASM and migrate to Docker if you need more capabilities:

```json
// Start simple with WASM
{
  "name": "data-processor",
  "kind": "wasm",
  "description": "Basic data processing"
}

// Migrate to Docker when you need system access
{
  "name": "data-processor-advanced", 
  "kind": "docker",
  "description": "Data processing with file I/O and database access"
}
```

## Summary

- **WASM**: Fast, lightweight, secure, but limited to pure computation
- **Docker**: Flexible, powerful, but larger and slower
- **Choose based on requirements**: Performance vs. capabilities
- **Start simple**: Begin with WASM, upgrade to Docker when needed
- **Consider the trade-offs**: Size, speed, security, and functionality
