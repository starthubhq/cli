# Starthub CLI Browser-Based Authentication

## Overview

The Starthub CLI now uses a secure, browser-based authentication flow instead of requiring users to enter credentials directly in the terminal. This provides better security and user experience.

## How It Works

### 1. **CLI Generates Code**
When you run `starthub login`, the CLI:
- Generates a unique 8-character authentication code
- Opens your browser to `https://editor.starthub.so/cli-auth?code=XXXX-XXXX`
- Displays the code in the terminal

### 2. **Browser Authentication**
In your browser:
- You're redirected to the Starthub editor
- The editor detects the CLI authentication flow
- You complete your login (email/password, OAuth, etc.)
- The editor displays the authentication code for you to copy

### 3. **Code Validation**
Back in the CLI:
- You paste the authentication code
- The CLI validates it against the Starthub backend
- If valid, authentication data is stored locally

## Security Features

- **Unique Codes**: Each authentication attempt generates a unique code
- **Time-Limited**: Codes expire after 10 minutes
- **Single-Use**: Each code can only be used once
- **No Credential Storage**: Passwords are never stored in the CLI
- **Secure Validation**: Codes are validated server-side via Supabase Edge Functions

## Database Schema

The system uses a new `cli_auth_codes` table:

```sql
CREATE TABLE cli_auth_codes (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  code TEXT UNIQUE NOT NULL,
  profile_id UUID,
  email TEXT NOT NULL,
  expires_at TIMESTAMPTZ NOT NULL,
  used_at TIMESTAMPTZ,
  created_at TIMESTAMPTZ DEFAULT now(),
  rls_owner_id UUID NOT NULL DEFAULT auth.uid()
);
```

## API Endpoints

### CLI Authentication Validation
- **Endpoint**: `POST /functions/v1/cli-auth`
- **Purpose**: Validate authentication codes
- **Input**: `{ "code": "XXXX-XXXX" }`
- **Output**: User profile information or error

## Usage Examples

### Basic Login
```bash
starthub login
```

### Custom API Base
```bash
starthub login --api-base https://staging-api.starthub.so
```

### Check Status
```bash
starthub auth
```

### Logout
```bash
starthub logout
```

## Authentication Flow Diagram

```
CLI                    Browser                    Backend
 |                        |                          |
 |-- generate code ------>|                          |
 |                        |-- open editor ---------->|
 |                        |                          |
 |                        |<-- user authenticates ---|
 |                        |                          |
 |                        |-- display code --------->|
 |                        |                          |
 |<-- user pastes code ---|                          |
 |                        |                          |
 |-- validate code ------>|                          |
 |                        |                          |
 |<-- validation result --|                          |
 |                        |                          |
 |-- store auth data -----|                          |
```

## Error Handling

### Common Issues

1. **Code Expired**: Codes expire after 10 minutes
   - Solution: Run `starthub login` again

2. **Invalid Code**: Code doesn't match what was generated
   - Solution: Copy the exact code from the terminal

3. **Browser Issues**: Can't open browser automatically
   - Solution: Manually navigate to the URL shown

4. **Network Issues**: Can't reach the backend
   - Solution: Check your internet connection and API base URL

## Development

### Local Testing

To test the authentication system locally:

1. **Deploy the migration**:
   ```bash
   cd api
   supabase db reset
   ```

2. **Deploy the Edge Function**:
   ```bash
   supabase functions deploy cli-auth
   ```

3. **Test the CLI**:
   ```bash
   cd ../cli
   cargo build --release
   ./target/release/starthub login
   ```

### Customization

You can customize the authentication system by:

- **Code Format**: Modify the `generate_auth_code()` function
- **Expiration Time**: Change the default 10-minute expiration
- **Editor URL**: Update the editor URL in the login command
- **Validation Logic**: Modify the Edge Function validation

## Troubleshooting

### Debug Mode
Run with verbose logging:
```bash
starthub login -v
```

### Check Logs
View Supabase Edge Function logs:
```bash
supabase functions logs cli-auth
```

### Reset Authentication
If you encounter issues:
```bash
starthub logout
starthub login
```

## Security Considerations

- **Code Generation**: Uses cryptographically secure random generation
- **Rate Limiting**: Consider implementing rate limiting on code generation
- **Audit Logging**: All authentication attempts are logged in the database
- **Cleanup**: Expired codes are automatically cleaned up

## Future Enhancements

- **QR Code Support**: Generate QR codes for mobile authentication
- **Push Notifications**: Send authentication requests to mobile apps
- **Biometric Support**: Integrate with system biometric authentication
- **Multi-Factor**: Add additional verification steps
