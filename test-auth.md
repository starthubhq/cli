# Starthub CLI Authentication Testing

## Overview
The Starthub CLI now includes a complete authentication system with the following commands:

- `starthub login` - Authenticate with Starthub backend
- `starthub logout` - Logout and remove stored credentials
- `starthub auth` - Check current authentication status

## How to Test

### 1. Check Initial Status
```bash
./target/release/starthub auth
```
Expected output: "❌ Not authenticated"

### 2. Test Login (Interactive)
```bash
./target/release/starthub login
```
This will prompt for:
- Email: Enter your Starthub account email
- Password: Enter your Starthub account password

### 3. Check Authentication Status
```bash
./target/release/starthub auth
```
Expected output: "✅ Authenticated with Starthub backend"

### 4. Test Logout
```bash
./target/release/starthub logout
```
Expected output: "✅ Successfully logged out!"

### 5. Verify Logout
```bash
./target/release/starthub auth
```
Expected output: "❌ Not authenticated"

## Authentication Storage

Credentials are stored securely in:
- **macOS**: `~/Library/Application Support/starthub/auth.json`
- **Linux**: `~/.config/starthub/auth.json`
- **Windows**: `%APPDATA%\starthub\auth.json`

## API Endpoints

The authentication system expects these endpoints:
- `POST /auth/login` - Login with email/password
- `GET /auth/me` - Validate current token

## Security Features

- Passwords are hidden during input
- Access tokens are stored securely in user config directory
- Tokens are validated on each auth status check
- Logout completely removes stored credentials

## Integration

The `load_auth_config()` function can be used by other parts of the CLI to:
- Get the current API base URL
- Get the current access token for authenticated API calls
- Check if the user is authenticated

## Example Usage in Code

```rust
use crate::commands::load_auth_config;

// Check if user is authenticated
if let Some((api_base, token)) = load_auth_config()? {
    // Make authenticated API call
    let client = reqwest::Client::new();
    let response = client
        .get(&format!("{}/api/endpoint", api_base))
        .header("Authorization", format!("Bearer {}", token))
        .send()
        .await?;
} else {
    // User needs to login
    anyhow::bail!("Please login first with 'starthub login'");
}
```
