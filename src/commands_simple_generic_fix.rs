/// Simple fix: Update the lookup_data_type_id function to handle generic types
/// by returning None (which sets data_type_id to null in the database)

// In the lookup_data_type_id function, add this check at the beginning:

async fn lookup_data_type_id(api_base: &str, type_name: &str, access_token: &str) -> anyhow::Result<Option<String>> {
    let client = reqwest::Client::new();
    
    // Check if this is a generic type - if so, return None (null data_type_id)
    if is_generic_type(type_name) {
        println!("🔧 Generic type '{}' detected - setting data_type_id to null", type_name);
        return Ok(None);
    }
    
    // ... rest of the existing function
}

/// Helper function to detect generic types
fn is_generic_type(type_name: &str) -> bool {
    // Single uppercase letter (T, K, V, etc.)
    if type_name.len() == 1 && type_name.chars().next().unwrap().is_uppercase() {
        return true;
    }
    
    // Contains angle brackets (type<T>, Array<T>, etc.)
    if type_name.contains('<') && type_name.contains('>') {
        return true;
    }
    
    // Specific generic patterns
    matches!(type_name, 
        "T" | "K" | "V" | "U" | "R" | "E" | "A" | "B" | "C" | "D" | "F" | "G" | "H" | "I" | "J" | "L" | "M" | "N" | "O" | "P" | "Q" | "S" | "W" | "X" | "Y" | "Z"
    )
}
