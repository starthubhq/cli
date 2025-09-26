## Complete Example: Weather Composition DAG

### Full Manifests

#### 1. get-weather-by-location-name (Top-level Composition)
```json
{
  "name": "get-weather-by-location-name",
  "description": "Get weather by location name",
  "version": "0.0.1",
  "kind": "composition",
  "manifest_version": 1,
  "repository": "github.com/tgirotto/get-weather-by-location-name",
  "license": "MIT",
  "inputs": {
    "weather_config": {
      "type": "WeatherConfig",
      "required": true
    }
  },
  "steps": {
    "get_coordinates": {
      "uses": "starthubhq/openweather-coordinates-by-location-name:0.0.1",
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
      },
      "inputs": {
        "open_weather_config": {
          "location_name": "{{inputs.weather_config.location_name}}",
          "open_weather_api_key": "{{inputs.weather_config.open_weather_api_key}}"
        }
      },
      "outputs": {
        "coordinates": "GeocodingResponse"
      }
    },
    "get_weather": {
      "uses": "starthubhq/openweather-current-weather:0.0.1",
      "types": {
        "WeatherResponse": {
          "coord": {
            "lon": "number",
            "lat": "number"
          },
          "weather": [
            {
              "id": "number",
              "main": "string",
              "description": "string",
              "icon": "string"
            }
          ],
          "base": "string",
          "main": {
            "temp": "number",
            "feels_like": "number",
            "temp_min": "number",
            "temp_max": "number",
            "pressure": "number",
            "humidity": "number",
            "sea_level": "number",
            "grnd_level": "number"
          },
          "visibility": "number",
          "wind": {
            "speed": "number",
            "deg": "number",
            "gust": "number"
          },
          "clouds": {
            "all": "number"
          },
          "dt": "number",
          "sys": {
            "country": "string",
            "sunrise": "number",
            "sunset": "number"
          },
          "timezone": "number",
          "id": "number",
          "name": "string",
          "cod": "number"
          }
        }
      },
      "inputs": {
        "weather_config": {
          "lat": "{{get_coordinates.coordinates.lat}}",
          "lon": "{{get_coordinates.coordinates.lon}}",
          "open_weather_api_key": "{{inputs.weather_config.open_weather_api_key}}"
        }
      },
      "outputs": {
        "weather": "WeatherResponse"
      }
    }
  },
  "outputs": {
    "response": {
      "description": "Simplified weather information for the location",
      "type": "CustomWeatherResponse",
      "value": {
        "location_name": "{{inputs.weather_config.location_name}}",
        "weather": "{{get_weather.weather.weather[0].description}}"
      }
    }
  },
  "types": {
    "WeatherConfig": {
      "location_name": {
        "description": "The name of the location to get weather for",
        "type": "string",
        "required": true
      },
      "open_weather_api_key": {
        "description": "OpenWeatherMap API key",
        "type": "string",
        "required": true
      }
    },
    "CustomWeatherResponse": [
      {
        "location_name": "string",
        "weather": "string"
      }
    ]
  }
}
```

#### 2. openweather-coordinates-by-location-name (Nested Composition)
```json
{
  "name": "openweather-coordinates-by-location-name",
  "description": "Get openweather coordinates by location name",
  "version": "0.0.1",
  "kind": "composition",
  "manifest_version": 1,
  "repository": "github.com/tgirotto/openweather-coordinates-by-location-name",
  "license": "MIT",
  "inputs": {
    "open_weather_config": {
      "type": "OpenWeatherConfig",
      "required": true
    }
  },
  "steps": {
    "get_geocoding_response": {
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
      "inputs": {
        "headers": {
          "Content-Type": "application/json",
          "Authorization": "Bearer {{inputs.open_weather_config.open_weather_api_key}}"
        },
        "url": "https://api.openweathermap.org/geo/1.0/direct?q={{inputs.open_weather_config.location_name}}&limit=1&appid={{inputs.open_weather_config.open_weather_api_key}}"
      },
      "outputs": {
        "response": "HttpResponse"
      }
    }
  },
  "outputs": {
    "coordinates": {
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
  },
  "types": {
    "OpenWeatherConfig": {
      "location_name": {
        "description": "The name of the location to get weather for",
        "type": "string",
        "required": true
      },
      "open_weather_api_key": {
        "description": "OpenWeatherMap API key",
        "type": "string",
        "required": true
      }
    },
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

#### 3. openweather-current-weather (Nested Composition)
```json
{
  "name": "openweather-current-weather",
  "description": "Get current weather by coordinates",
  "version": "0.0.1",
  "kind": "composition",
  "manifest_version": 1,
  "repository": "github.com/tgirotto/openweather-current-weather",
  "license": "MIT",
  "inputs": {
    "weather_config": {
      "type": "WeatherConfig",
      "required": true
    }
  },
  "steps": {
    "get_weather_response": {
      "uses": "starthubhq/http-get-wasm:0.0.16",
      "types": {
        "HttpResponse": {
          "status": "number",
          "body": "string"
        }
      },
      "inputs": {
        "url": "https://api.openweathermap.org/data/2.5/weather?lat={{inputs.weather_config.lat}}&lon={{inputs.weather_config.lon}}&appid={{inputs.weather_config.open_weather_api_key}}"
      },
      "outputs": {
        "response": "HttpResponse"
      }
    }
  },
  "outputs": {
    "weather": {
      "type": "WeatherResponse",
      "value": {
        "coord": {
          "lon": "{{get_weather_response.body.coord.lon}}",
          "lat": "{{get_weather_response.body.coord.lat}}"
        },
        "weather": [
          {
            "id": "{{get_weather_response.body.weather[0].id}}",
            "main": "{{get_weather_response.body.weather[0].main}}",
            "description": "{{get_weather_response.body.weather[0].description}}",
            "icon": "{{get_weather_response.body.weather[0].icon}}"
          }
        ],
        "base": "{{get_weather_response.body.base}}",
        "main": {
          "temp": "{{get_weather_response.body.main.temp}}",
          "feels_like": "{{get_weather_response.body.main.feels_like}}",
          "temp_min": "{{get_weather_response.body.main.temp_min}}",
          "temp_max": "{{get_weather_response.body.main.temp_max}}",
          "pressure": "{{get_weather_response.body.main.pressure}}",
          "humidity": "{{get_weather_response.body.main.humidity}}",
          "sea_level": "{{get_weather_response.body.main.sea_level}}",
          "grnd_level": "{{get_weather_response.body.main.grnd_level}}"
        },
        "visibility": "{{get_weather_response.body.visibility}}",
        "wind": {
          "speed": "{{get_weather_response.body.wind.speed}}",
          "deg": "{{get_weather_response.body.wind.deg}}",
          "gust": "{{get_weather_response.body.wind.gust}}"
        },
        "clouds": {
          "all": "{{get_weather_response.body.clouds.all}}"
        },
        "dt": "{{get_weather_response.body.dt}}",
        "sys": {
          "country": "{{get_weather_response.body.sys.country}}",
          "sunrise": "{{get_weather_response.body.sys.sunrise}}",
          "sunset": "{{get_weather_response.body.sys.sunset}}"
        },
        "timezone": "{{get_weather_response.body.timezone}}",
        "id": "{{get_weather_response.body.id}}",
        "name": "{{get_weather_response.body.name}}",
        "cod": "{{get_weather_response.body.cod}}"
      }
    }
  },
  "types": {
    "WeatherConfig": {
      "lat": {
        "description": "Latitude coordinate",
        "type": "number",
        "required": true
      },
      "lon": {
        "description": "Longitude coordinate",
        "type": "number",
        "required": true
      },
      "open_weather_api_key": {
        "description": "OpenWeatherMap API key",
        "type": "string",
        "required": true
      }
    },
    "WeatherResponse": {
      "coord": {
        "lon": "number",
        "lat": "number"
      },
      "weather": [
        {
          "id": "number",
          "main": "string",
          "description": "string",
          "icon": "string"
        }
      ],
      "base": "string",
      "main": {
        "temp": "number",
        "feels_like": "number",
        "temp_min": "number",
        "temp_max": "number",
        "pressure": "number",
        "humidity": "number",
        "sea_level": "number",
        "grnd_level": "number"
      },
      "visibility": "number",
      "wind": {
        "speed": "number",
        "deg": "number",
        "gust": "number"
      },
      "clouds": {
        "all": "number"
      },
      "dt": "number",
      "sys": {
        "country": "string",
        "sunrise": "number",
        "sunset": "number"
      },
      "timezone": "number",
      "id": "number",
      "name": "string",
      "cod": "number"
    }
  }
}
```

#### 4. http-get-wasm (Atomic WASM Module)
```json
{
  "name": "http-get-wasm",
  "description": "HTTP GET request module for fetching data from web APIs",
  "version": "0.0.16",
  "kind": "wasm",
  "manifest_version": 1,
  "repository": "github.com/starthubhq/http-get-wasm",
  "license": "MIT",
  "inputs": {
    "url": {
      "description": "The URL to fetch data from",
      "type": "string",
      "required": true,
      "default": null
    },
    "headers": {
      "description": "Optional HTTP headers to send with the request",
      "type": "HttpHeaders",
      "required": false,
      "default": null
    }
  },
  "outputs": {
    "response": {
      "description": "HTTP response from the request",
      "type": "HttpResponse",
      "required": true
    }
  },
  "types": {
    "HttpHeaders": {
      "Accept": "string",
      "Authorization": "string",
      "Content-Type": "string",
      "User-Agent": "string",
      "X-API-Key": "string"
    },
    "HttpResponse": {
      "body": "string",
      "status": "number"
    }
  }
}
```

### Input Manifest
Starting with the top-level composition `get-weather-by-location-name`:

```json
{
  "inputs": {
    "weather_config": {
      "location_name": "Rome",
      "open_weather_api_key": "c646e7e39a20317bd056f6ff501d2ab2"
    }
  }
}
```

### Step 1: Composition Analysis
The composition has two steps:
1. `get_coordinates` → uses `openweather-coordinates-by-location-name:0.0.1`
2. `get_weather` → uses `openweather-current-weather:0.0.1`

### Step 2: First Recursion - get_coordinates
Expanding `openweather-coordinates-by-location-name`:

```json
{
  "name": "openweather-coordinates-by-location-name",
  "kind": "composition",
  "steps": {
    "get_geocoding_response": {
      "uses": "starthubhq/http-get-wasm:0.0.16",
      "inputs": {
        "headers": {
          "Content-Type": "application/json",
          "Authorization": "Bearer {{inputs.open_weather_config.open_weather_api_key}}"
        },
        "url": "https://api.openweathermap.org/geo/1.0/direct?q={{inputs.open_weather_config.location_name}}&limit=1&appid={{inputs.open_weather_config.open_weather_api_key}}"
      }
    }
  }
}
```

**Variable Mapping for get_coordinates:**
```rust
VariableMapping {
  target_step: "get_coordinates",
  variable_name: "open_weather_config",
  source_step: "inputs",
  source_path: "weather_config",
  target_path: "open_weather_config"
}
```

### Step 3: Second Recursion - get_weather
Expanding `openweather-current-weather`:

```json
{
  "name": "openweather-current-weather", 
  "kind": "composition",
  "steps": {
    "get_weather_response": {
      "uses": "starthubhq/http-get-wasm:0.0.16",
      "inputs": {
        "url": "https://api.openweathermap.org/data/2.5/weather?lat={{inputs.weather_config.lat}}&lon={{inputs.weather_config.lon}}&appid={{inputs.weather_config.open_weather_api_key}}"
      }
    }
  }
}
```

**Variable Mapping for get_weather:**
```rust
VariableMapping {
  target_step: "get_weather",
  variable_name: "weather_config",
  source_step: "get_coordinates",
  source_path: "coordinates",
  target_path: "weather_config"
}
```

### Step 4: Final DAG Construction

The complete execution state after DAG construction:

```rust
ExecutionState {
  inputs: {
    "weather_config": {
      "location_name": "Rome",
      "open_weather_api_key": "c646e7e39a20317bd056f6ff501d2ab2"
    }
  },
  steps: [
    StepState {
      id: "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
      original_name: "get_geocoding_response",
      uses: "starthubhq/http-get-wasm:0.0.16",
      kind: "wasm",
      inputs: {
        "headers": {
          "Content-Type": "application/json",
          "Authorization": "Bearer {{inputs.weather_config.open_weather_api_key}}"
        },
        "url": "https://api.openweathermap.org/geo/1.0/direct?q={{inputs.weather_config.location_name}}&limit=1&appid={{inputs.weather_config.open_weather_api_key}}"
      },
      outputs: {}
    },
    StepState {
      id: "b2c3d4e5-f6g7-8901-bcde-f23456789012",
      original_name: "get_weather_response", 
      uses: "starthubhq/http-get-wasm:0.0.16",
      kind: "wasm",
      inputs: {
        "url": "https://api.openweathermap.org/data/2.5/weather?lat={{get_coordinates.coordinates.lat}}&lon={{get_coordinates.coordinates.lon}}&appid={{inputs.weather_config.open_weather_api_key}}"
      },
      outputs: {}
    }
  ],
  data_flow: [
    DataFlowEdge {
      from_step: "inputs",
      to_step: "get_geocoding_response",
      variable_name: "weather_config",
      source_path: "weather_config",
      target_path: "open_weather_config"
    },
    DataFlowEdge {
      from_step: "get_geocoding_response", 
      to_step: "get_weather_response",
      variable_name: "coordinates",
      source_path: "response",
      target_path: "weather_config"
    }
  ]
}
```

### Step 5: Variable Resolution Process

#### Input Resolution
```rust
// Resolve inputs.weather_config.location_name
resolve_variable("{{inputs.weather_config.location_name}}") 
// → "Rome"

// Resolve inputs.weather_config.open_weather_api_key  
resolve_variable("{{inputs.weather_config.open_weather_api_key}}")
// → "c646e7e39a20317bd056f6ff501d2ab2"
```

#### Step-to-Step Resolution
```rust
// After get_geocoding_response executes, resolve coordinates
resolve_variable("{{get_coordinates.coordinates.lat}}")
// → 41.9028 (from geocoding API response)

resolve_variable("{{get_coordinates.coordinates.lon}}") 
// → 12.4964 (from geocoding API response)
```

### Step 6: Execution Order

The DAG execution follows this order:

1. **get_geocoding_response** executes first
   - Input: `{{inputs.weather_config.location_name}}` → "Rome"
   - Input: `{{inputs.weather_config.open_weather_api_key}}` → "c646e7e39a20317bd056f6ff501d2ab2"
   - Output: Geocoding response with lat/lon coordinates

2. **get_weather_response** executes second
   - Input: `{{get_coordinates.coordinates.lat}}` → 41.9028
   - Input: `{{get_coordinates.coordinates.lon}}` → 12.4964  
   - Input: `{{inputs.weather_config.open_weather_api_key}}` → "c646e7e39a20317bd056f6ff501d2ab2"
   - Output: Weather data for Rome

### Step 7: Final Output

The composition returns:
```json
{
  "response": {
    "location_name": "Rome",
    "weather": "clear sky"
  }
}
```

## Key Implementation Details

### Variable Template Resolution
- `{{inputs.weather_config.location_name}}` → Direct input access
- `{{get_coordinates.coordinates.lat}}` → Step output access
- `{{inputs.weather_config.open_weather_api_key}}` → Reused input

### Data Flow Dependencies
- `get_weather_response` depends on `get_geocoding_response` completion
- Both steps can access original inputs
- Variable resolution happens at execution time

### Step Name Preservation
- `original_name` maintains composition step names for variable resolution
- `id` provides unique execution tracking
- `uses` specifies the actual WASM module to execute

## DAG Visualization

### ASCII Diagram: Weather Composition DAG

```
┌─────────────────────────────────────────────────────────────────────────────────┐
│                           INPUT LAYER                                          │
└─────────────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────────────┐
│                        weather_config: {                                        │
│                          location_name: "Rome",                                 │
│                          open_weather_api_key: "c646e7e39a20317bd056f6ff501d2ab2" │
│                        }                                                        │
└─────────────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────────────┐
│                        COMPOSITION LAYER                                        │
│                                                                                 │
│  ┌─────────────────────────────────┐    ┌─────────────────────────────────┐    │
│  │    get-weather-by-location-name │    │    get-weather-by-location-name │    │
│  │                                 │    │                                 │    │
│  │  ┌─────────────────────────────┐│    │┌─────────────────────────────┐  │    │
│  │  │ get_coordinates              ││    ││ get_weather                │  │    │
│  │  │ uses: openweather-coordinates││    ││ uses: openweather-current  │  │    │
│  │  │ -by-location-name:0.0.1      ││    ││ -weather:0.0.1             │  │    │
│  │  └─────────────────────────────┘│    │└─────────────────────────────┘  │    │
│  └─────────────────────────────────┘    └─────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────────────┐
│                        RECURSION LAYER 1                                        │
│                                                                                 │
│  ┌─────────────────────────────────────────────────────────────────────────────┐│
│  │              openweather-coordinates-by-location-name                      ││
│  │                                                                             ││
│  │  ┌─────────────────────────────────────────────────────────────────────────┐││
│  │  │ get_geocoding_response                                                  │││
│  │  │ uses: starthubhq/http-get-wasm:0.0.16                                  │││
│  │  │                                                                         │││
│  │  │ Inputs:                                                                 │││
│  │  │ • url: "https://api.openweathermap.org/geo/1.0/direct?q={{location}}&..." │││
│  │  │ • headers: { "Authorization": "Bearer {{api_key}}" }                  │││
│  │  └─────────────────────────────────────────────────────────────────────────┘││
│  └─────────────────────────────────────────────────────────────────────────────┘│
└─────────────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────────────┐
│                        RECURSION LAYER 2                                        │
│                                                                                 │
│  ┌─────────────────────────────────────────────────────────────────────────────┐│
│  │              openweather-current-weather                                   ││
│  │                                                                             ││
│  │  ┌─────────────────────────────────────────────────────────────────────────┐││
│  │  │ get_weather_response                                                    │││
│  │  │ uses: starthubhq/http-get-wasm:0.0.16                                  │││
│  │  │                                                                         │││
│  │  │ Inputs:                                                                 │││
│  │  │ • url: "https://api.openweathermap.org/data/2.5/weather?lat={{lat}}&..." │││
│  │  └─────────────────────────────────────────────────────────────────────────┘││
│  └─────────────────────────────────────────────────────────────────────────────┘│
└─────────────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────────────┐
│                        FINAL DAG LAYER                                          │
│                                                                                 │
│  ┌─────────────────────────────────────────────────────────────────────────────┐│
│  │                           EXECUTION STATE                                  ││
│  │                                                                             ││
│  │  ┌─────────────────────────────────────────────────────────────────────────┐││
│  │  │ Step 1: get_geocoding_response                                         │││
│  │  │ ID: a1b2c3d4-e5f6-7890-abcd-ef1234567890                              │││
│  │  │ Uses: starthubhq/http-get-wasm:0.0.16                                  │││
│  │  │ Kind: wasm                                                              │││
│  │  │                                                                         │││
│  │  │ Inputs:                                                                 │││
│  │  │ • url: "https://api.openweathermap.org/geo/1.0/direct?q=Rome&..."      │││
│  │  │ • headers: { "Authorization": "Bearer c646e7e39a20317bd056f6ff501d2ab2" } │││
│  │  └─────────────────────────────────────────────────────────────────────────┘││
│  │                                    │                                       ││
│  │                                    ▼                                       ││
│  │  ┌─────────────────────────────────────────────────────────────────────────┐││
│  │  │ Step 2: get_weather_response                                           │││
│  │  │ ID: b2c3d4e5-f6g7-8901-bcde-f23456789012                              │││
│  │  │ Uses: starthubhq/http-get-wasm:0.0.16                                  │││
│  │  │ Kind: wasm                                                              │││
│  │  │                                                                         │││
│  │  │ Inputs:                                                                 │││
│  │  │ • url: "https://api.openweathermap.org/data/2.5/weather?lat=41.9028&..." │││
│  │  └─────────────────────────────────────────────────────────────────────────┘││
│  └─────────────────────────────────────────────────────────────────────────────┘│
└─────────────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────────────┐
│                        DATA FLOW EDGES                                          │
│                                                                                 │
│  ┌─────────────────────────────────────────────────────────────────────────────┐│
│  │ DataFlowEdge 1:                                                             ││
│  │ from_step: "inputs"                                                         ││
│  │ to_step: "get_geocoding_response"                                           ││
│  │ variable_name: "weather_config"                                             ││
│  │ source_path: "weather_config"                                                ││
│  │ target_path: "open_weather_config"                                           ││
│  └─────────────────────────────────────────────────────────────────────────────┘│
│                                                                                 │
│  ┌─────────────────────────────────────────────────────────────────────────────┐│
│  │ DataFlowEdge 2:                                                             ││
│  │ from_step: "get_geocoding_response"                                         ││
│  │ to_step: "get_weather_response"                                             ││
│  │ variable_name: "coordinates"                                                ││
│  │ source_path: "response"                                                      ││
│  │ target_path: "weather_config"                                               ││
│  └─────────────────────────────────────────────────────────────────────────────┘│
└─────────────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────────────┐
│                        EXECUTION FLOW                                           │
│                                                                                 │
│  ┌─────────────────────────────────────────────────────────────────────────────┐│
│  │ 1. Execute get_geocoding_response                                          ││
│  │    Input: {{inputs.weather_config.location_name}} → "Rome"                 ││
│  │    Input: {{inputs.weather_config.open_weather_api_key}} → "c646e7e39a..." ││
│  │    Output: { lat: 41.9028, lon: 12.4964, ... }                            ││
│  └─────────────────────────────────────────────────────────────────────────────┘│
│                                    │                                           │
│                                    ▼                                           │
│  ┌─────────────────────────────────────────────────────────────────────────────┐│
│  │ 2. Execute get_weather_response                                            ││
│  │    Input: {{get_coordinates.coordinates.lat}} → 41.9028                   ││
│  │    Input: {{get_coordinates.coordinates.lon}} → 12.4964                   ││
│  │    Input: {{inputs.weather_config.open_weather_api_key}} → "c646e7e39a..." ││
│  │    Output: { weather: "clear sky", ... }                                  ││
│  └─────────────────────────────────────────────────────────────────────────────┘│
└─────────────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────────────┐
│                        FINAL OUTPUT                                             │
│                                                                                 │
│  ┌─────────────────────────────────────────────────────────────────────────────┐│
│  │ {                                                                          ││
│  │   "response": {                                                            ││
│  │     "location_name": "Rome",                                               ││
│  │     "weather": "clear sky"                                                 ││
│  │   }                                                                        ││
│  │ }                                                                          ││
│  └─────────────────────────────────────────────────────────────────────────────┘│
└─────────────────────────────────────────────────────────────────────────────────┘
```

### Variable Resolution Flow

```
┌─────────────────────────────────────────────────────────────────────────────────┐
│                        VARIABLE RESOLUTION FLOW                                 │
└─────────────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────────────┐
│                        TEMPLATE VARIABLES                                       │
│                                                                                 │
│  {{inputs.weather_config.location_name}}                                       │
│  {{inputs.weather_config.open_weather_api_key}}                                │
│  {{get_coordinates.coordinates.lat}}                                           │
│  {{get_coordinates.coordinates.lon}}                                           │
└─────────────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────────────┐
│                        RESOLUTION PROCESS                                       │
│                                                                                 │
│  Step 1: Input Resolution                                                       │
│  ┌─────────────────────────────────────────────────────────────────────────────┐│
│  │ {{inputs.weather_config.location_name}} → "Rome"                           ││
│  │ {{inputs.weather_config.open_weather_api_key}} → "c646e7e39a20317bd056f6ff501d2ab2" ││
│  └─────────────────────────────────────────────────────────────────────────────┘│
│                                    │                                           │
│                                    ▼                                           │
│  Step 2: Step-to-Step Resolution                                               │
│  ┌─────────────────────────────────────────────────────────────────────────────┐│
│  │ {{get_coordinates.coordinates.lat}} → 41.9028                             ││
│  │ {{get_coordinates.coordinates.lon}} → 12.4964                             ││
│  └─────────────────────────────────────────────────────────────────────────────┘│
└─────────────────────────────────────────────────────────────────────────────────┘
```

### Execution Timeline

```
┌─────────────────────────────────────────────────────────────────────────────────┐
│                        EXECUTION TIMELINE                                       │
└─────────────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────────────┐
│  Time 0: DAG Construction                                                       │
│  ┌─────────────────────────────────────────────────────────────────────────────┐│
│  │ • Parse composition manifest                                               ││
│  │ • Recursively expand composition steps                                    ││
│  │ • Build variable dependency graph                                          ││
│  │ • Create execution state with atomic steps                                 ││
│  └─────────────────────────────────────────────────────────────────────────────┘│
└─────────────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────────────┐
│  Time 1: Execute get_geocoding_response                                        │
│  ┌─────────────────────────────────────────────────────────────────────────────┐│
│  │ • Resolve input variables                                                  ││
│  │ • Download http-get-wasm:0.0.16 artifact                                   ││
│  │ • Execute WASM module with resolved inputs                                 ││
│  │ • Store output: { lat: 41.9028, lon: 12.4964, ... }                      ││
│  └─────────────────────────────────────────────────────────────────────────────┘│
└─────────────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────────────┐
│  Time 2: Execute get_weather_response                                          │
│  ┌─────────────────────────────────────────────────────────────────────────────┐│
│  │ • Resolve step output variables                                            ││
│  │ • Reuse http-get-wasm:0.0.16 artifact                                     ││
│  │ • Execute WASM module with resolved inputs                                 ││
│  │ • Store output: { weather: "clear sky", ... }                             ││
│  └─────────────────────────────────────────────────────────────────────────────┘│
└─────────────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────────────┐
│  Time 3: Final Output                                                           │
│  ┌─────────────────────────────────────────────────────────────────────────────┐│
│  │ • Aggregate all step outputs                                               ││
│  │ • Apply composition output mapping                                          ││
│  │ • Return final result: { "response": { "location_name": "Rome", "weather": "clear sky" } } ││
│  └─────────────────────────────────────────────────────────────────────────────┘│
└─────────────────────────────────────────────────────────────────────────────────┘
```


