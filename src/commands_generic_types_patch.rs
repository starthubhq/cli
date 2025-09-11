/// Helper function to detect if a type name represents a generic type
fn is_generic_type(type_name: &str) -> bool {
    // Check for common generic type patterns:
    // - Single letter types like "T", "K", "V"
    // - Type constructors like "type<T>", "Array<T>", "Map<K,V>"
    // - Generic patterns with angle brackets
    
    if type_name.len() == 1 && type_name.chars().next().unwrap().is_uppercase() {
        // Single uppercase letter (T, K, V, etc.)
        return true;
    }
    
    if type_name.contains('<') && type_name.contains('>') {
        // Contains angle brackets (type<T>, Array<T>, etc.)
        return true;
    }
    
    // Check for specific generic patterns
    matches!(type_name, 
        "T" | "K" | "V" | "U" | "R" | "E" | "A" | "B" | "C" | "D" | "F" | "G" | "H" | "I" | "J" | "L" | "M" | "N" | "O" | "P" | "Q" | "S" | "W" | "X" | "Y" | "Z"
    )
}

/// Looks up data_type_id from the data_types table for a given type name
async fn lookup_data_type_id(api_base: &str, type_name: &str, access_token: &str) -> anyhow::Result<Option<String>> {
    let client = reqwest::Client::new();
    
    // Check if this is a generic type first
    if is_generic_type(type_name) {
        // For generic types, return the "generic" primitive type ID
        let response = client
            .get(&format!("{}/rest/v1/data_types?select=id&name=eq.generic&is_primitive=eq.true", 
                api_base))
            .header("apikey", crate::config::SUPABASE_ANON_KEY)
            .header("Authorization", format!("Bearer {}", access_token))
            .send()
            .await?;
        
        if response.status().is_success() {
            let data_types: Vec<serde_json::Value> = response.json().await?;
            if let Some(data_type) = data_types.first() {
                if let Some(id) = data_type["id"].as_str() {
                    return Ok(Some(id.to_string()));
                }
            }
        }
        return Ok(None);
    }
    
    // First try to find as a primitive type
    let response = client
        .get(&format!("{}/rest/v1/data_types?select=id&name=eq.{}&is_primitive=eq.true", 
            api_base, type_name))
        .header("apikey", crate::config::SUPABASE_ANON_KEY)
        .header("Authorization", format!("Bearer {}", access_token))
        .send()
        .await?;
    
    if response.status().is_success() {
        let data_types: Vec<serde_json::Value> = response.json().await?;
        if let Some(data_type) = data_types.first() {
            if let Some(id) = data_type["id"].as_str() {
                return Ok(Some(id.to_string()));
            }
        }
    }
    
    // If not found as primitive, try case-insensitive search for custom types
    let response = client
        .get(&format!("{}/rest/v1/data_types?select=id&name=ilike.{}&is_primitive=eq.false", 
            api_base, type_name))
        .header("apikey", crate::config::SUPABASE_ANON_KEY)
        .header("Authorization", format!("Bearer {}", access_token))
        .send()
        .await?;
    
    if response.status().is_success() {
        let data_types: Vec<serde_json::Value> = response.json().await?;
        if let Some(data_type) = data_types.first() {
            if let Some(id) = data_type["id"].as_str() {
                return Ok(Some(id.to_string()));
            }
        }
    }
    
    Ok(None)
}
