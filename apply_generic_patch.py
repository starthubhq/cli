#!/usr/bin/env python3

import re

# Read the file
with open('src/commands.rs', 'r') as f:
    content = f.read()

# Add the helper function before the lookup_data_type_id function
helper_function = '''/// Helper function to detect if a type name represents a generic type
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

'''

# Find the position to insert the helper function
insert_pos = content.find('/// Looks up data_type_id from the data_types table for a given type name')
content = content[:insert_pos] + helper_function + content[insert_pos:]

# Add the generic type check at the beginning of the lookup function
generic_check = '''    // Check if this is a generic type first
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
    
'''

# Find the position to insert the generic check
insert_pos = content.find('    let client = reqwest::Client::new();')
insert_pos = content.find('\n', insert_pos) + 1
content = content[:insert_pos] + generic_check + content[insert_pos:]

# Write the modified content back
with open('src/commands.rs', 'w') as f:
    f.write(content)

print("Successfully applied generic types patch!")
