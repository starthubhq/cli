---
sidebar_position: 4
---

# login

Authenticate with the Starthub backend.

## Usage

```bash
starthub login [--api-base <url>]
```

## Options

- `--api-base <url>` - Starthub API base URL (default: `https://api.starthub.so`)

## Description

The `login` command initiates the authentication process with Starthub. It opens your default web browser to the Starthub editor authentication page where you can complete the login process.

## Authentication Flow

1. The command opens your browser to `https://registry.starthub.so/cli-auth`
2. Complete the authentication in your browser
3. Your authentication token will be stored locally for future CLI operations

## Example

```bash
starthub login
```

or with a custom API base:

```bash
starthub login --api-base https://api.starthub.so
```

## Notes

- The authentication token is stored in your system's config directory
- You can check your authentication status using `starthub auth`
- To logout, use `starthub logout`
