#!/bin/bash

# Test script for the new composition ID functionality
# This script demonstrates how to run a composition using its ID

echo "Testing composition ID functionality..."

# Set the API base URL (adjust as needed)
export STARTHUB_API="https://api.starthub.so"

# Set your token if needed (optional)
# export STARTHUB_TOKEN="your-token-here"

# Test with a composition ID
# Replace "your-composition-id" with an actual composition ID
COMPOSITION_ID="your-composition-id"

echo "Running composition with ID: $COMPOSITION_ID"

# Run the composition
# The CLI will now:
# 1. Detect that this is a composition ID
# 2. Fetch the starthub.json from Supabase storage
# 3. Parse the composition and execute the steps
./target/debug/starthub run "$COMPOSITION_ID" --runner local

echo "Test completed!"
