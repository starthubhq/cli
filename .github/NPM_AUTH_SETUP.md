# npm Authentication Setup for GitHub Actions

As of December 9, 2025, npm has deprecated classic tokens and introduced new authentication methods. This document explains how to set up authentication for the `@starthub/cli` package deployment.

## What Changed?

- **Classic tokens** have been permanently revoked
- **New options**: Granular access tokens or OIDC trusted publishing
- Session-based auth (2-hour tokens) for local development only

## Setup Instructions

### Option 1: Granular Access Token (Current Implementation)

This workflow uses granular access tokens stored as GitHub secrets. **You need to create and rotate these tokens regularly.**

#### Creating a New Token

**Via CLI:**
```bash
npm token create \
  --type=granular \
  --description="GitHub Actions - starthub/cli release workflow" \
  --access=read-write \
  --scope=@starthub
```

**Via Web UI:**
1. Go to [npmjs.com/settings/~/tokens](https://npmjs.com/settings/~/tokens)
2. Click "Generate New Token" → "Granular Access Token"
3. Configure:
   - **Name**: `GitHub Actions - starthub/cli`
   - **Expiration**: 90 days (maximum for write tokens)
   - **Packages and scopes**: Select `@starthub/cli` with **Read and Write** permissions
   - **Organizations**: Select your organization if applicable
   - **IP ranges**: Leave empty (GitHub Actions uses dynamic IPs)
   - ⚠️ **IMPORTANT**: Enable **"Bypass 2FA requirement"** for CI/CD to work

#### Adding Token to GitHub

1. Copy the generated token (it will only be shown once)
2. Go to your repository settings: `https://github.com/starthubhq/cli/settings/secrets/actions`
3. Update the secret named `NPM_TOKEN` with the new token value
4. Save

#### Token Rotation

⚠️ **Granular write tokens expire after max 90 days**. Set a reminder to:
1. Generate a new token before expiration
2. Update the `NPM_TOKEN` GitHub secret
3. Delete the old token from npm

### Option 2: OIDC Trusted Publishing (Future - Most Secure)

OIDC eliminates the need to manage tokens entirely. When npm fully supports this for your package:

#### Prerequisites
- npm must support OIDC trusted publishing for your package
- Your package must be configured on npm's side

#### Workflow Changes

Update the workflow to remove the `NODE_AUTH_TOKEN`:

```yaml
- name: Publish to npm with provenance
  run: npm publish --access public --provenance
  # No NODE_AUTH_TOKEN needed with OIDC
```

The `id-token: write` permission is already configured in the workflow.

#### Configuration

1. Visit your package settings on npm
2. Configure GitHub Actions as a trusted publisher
3. Provide your GitHub repository details: `starthubhq/cli`

## Current Workflow Status

✅ Workflow updated to include `--provenance` flag  
✅ Permissions include `id-token: write` (required for provenance and OIDC)  
⚠️ **Action Required**: Create a new granular access token and update `NPM_TOKEN` secret

## Troubleshooting

### "401 Unauthorized" Error
- Your token may have expired (check token expiration date)
- Token may not have write permissions for `@starthub/cli`
- "Bypass 2FA" may not be enabled on the token

### "403 Forbidden" Error
- Token doesn't have access to the `@starthub` scope
- Package name mismatch in permissions

### "Two-Factor Authentication Required" Error
- The "Bypass 2FA" option was not enabled when creating the token
- Create a new token with this option enabled

## References

- [npm Token Management Documentation](https://docs.npmjs.com/creating-and-viewing-access-tokens)
- [GitHub Blog: npm Authentication Changes](https://github.blog/changelog/2025-12-09-npm-classic-tokens-revoked-session-based-auth-and-cli-token-management-now-available/)
- [npm Provenance Documentation](https://docs.npmjs.com/generating-provenance-statements)

