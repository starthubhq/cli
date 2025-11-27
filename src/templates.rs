// ---------- Docker scaffolding templates ----------
pub const DOCKERFILE_TPL: &str = r#"FROM alpine:3.20

RUN apk add --no-cache curl jq

WORKDIR /app
COPY entrypoint.sh /app/entrypoint.sh
RUN chmod +x /app/entrypoint.sh

CMD ["/app/entrypoint.sh"]
"#;

pub const ENTRYPOINT_SH_TPL: &str = r#"#!/bin/sh
set -euo pipefail

# Read entire JSON payload from stdin:
INPUT="$(cat || true)"

# Secrets from env first; otherwise from stdin.params (avoid leaking in logs/state)
do_access_token="${do_access_token:-}"
if [ -z "${do_access_token}" ]; then
  do_access_token="$(printf '%s' "$INPUT" | jq -r '(.params.do_access_token // .do_access_token // empty)')"
fi

# Non-secrets from env or stdin.params
do_project_id="${do_project_id:-$(printf '%s' "$INPUT" | jq -r '(.params.do_project_id // .do_project_id // empty)')}"
do_tag_name="${do_tag_name:-$(printf '%s' "$INPUT" | jq -r '(.params.do_tag_name // .do_tag_name // empty)')}"

# Validate
[ -n "${do_access_token:-}" ] || { echo "Error: do_access_token missing (env or stdin.params)" >&2; exit 1; }
[ -n "${do_project_id:-}" ]  || { echo "Error: do_project_id missing (env or stdin.params)"  >&2; exit 1; }
[ -n "${do_tag_name:-}" ]    || { echo "Error: do_tag_name missing (env or stdin.params)"    >&2; exit 1; }

label="starthub-tag:${do_tag_name}"
echo "📝 Updating project ${do_project_id} description to include '${label}'..." >&2

# 1) Fetch current project
get_resp="$(
  curl -sS -f -X GET "https://api.digitalocean.com/v2/projects/${do_project_id}" \
    -H "Authorization: Bearer ${do_access_token}" \
    -H "Content-Type: application/json"
)"

current_desc="$(printf '%s' "$get_resp" | jq -r '.project.description // ""')"

# 2) Build new description idempotently
case "$current_desc" in
  *"$label"*) new_desc="$current_desc" ;;
  "")         new_desc="$label" ;;
  *)          new_desc="$current_desc, $label" ;;
esac

# 3) PATCH only if needed
if [ "$new_desc" = "$current_desc" ]; then
  patch_resp="$get_resp"
else
  patch_resp="$(
    curl -sS -f -X PATCH "https://api.digitalocean.com/v2/projects/${do_project_id}" \
      -H "Authorization: Bearer ${do_access_token}" \
      -H "Content-Type: application/json" \
      -d "$(jq -nc --arg d "$new_desc" '{description:$d}')"
  )"
fi

# 4) Verify success
project_id_parsed="$(printf '%s' "$patch_resp" | jq -r '.project.id // empty')"
[ -n "$project_id_parsed" ] || { echo "❌ Failed to update project"; echo "$patch_resp" | jq . >&2; exit 1; }

# 5) ✅ Emit output that matches the manifest exactly
echo "::starthub:state::{\"do_tag_name\":\"${do_tag_name}\"}"

# 6) Human-readable logs to STDERR
{
  echo "✅ Tag ensured in description. Project ID: ${project_id_parsed}"
  echo "Final description:"
  printf '%s\n' "$patch_resp" | jq -r '.project.description // ""'
} >&2
"#;

pub const GITIGNORE_TPL: &str = r#"/target
/dist
*.log
*.tmp
.DS_Store
.env
.env.*
"#;

pub const DOCKERIGNORE_TPL: &str = r#"*
!entrypoint.sh
!starthub.json
!.dockerignore
!.gitignore
!README.md
!Dockerfile
"#;

// ---------- WASM scaffolding templates ----------
pub const WASM_MAIN_RS_TPL: &str = r#"use std::io::{self, Read};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};
use waki::Client;

#[derive(Deserialize)]
struct Input {
    #[serde(default)]
    state: Value,
    #[serde(default)]
    params: Value,
}

fn main() {
    // ---- read stdin ----
    let mut buf = String::new();
    let _ = io::stdin().read_to_string(&mut buf);
    let input: Input = serde_json::from_str(&buf)
        .unwrap_or(Input { state: Value::Null, params: Value::Null });

    // ---- required url ----
    let Some(url) = input.params.get("url").and_then(|v| v.as_str()) else {
        eprintln!("Error: missing required param 'url'");
        return;
    };

    // ---- optional headers (make &'static strs) ----
    let mut headers_static: Vec<(&'static str, &'static str)> = Vec::new();
    if let Some(hmap) = input.params.get("headers").and_then(|v| v.as_object()) {
        for (k, v) in hmap {
            if let Some(val) = v.as_str() {
                let k_static: &'static str = Box::leak(k.clone().into_boxed_str());
                let v_static: &'static str = Box::leak(val.to_string().into_boxed_str());
                headers_static.push((k_static, v_static));
            }
        }
    }

    // ---- GET ----
    let resp = Client::new()
        .get(url)
        .headers(headers_static) // <-- pass Vec, not slice
        .connect_timeout(Duration::from_secs(15))
        .send();

    match resp {
        Ok(r) => {
            let status = r.status_code();
            let body = r.body().unwrap_or_default();
            let body_str = String::from_utf8_lossy(&body).to_string();

            // Emit manifest-style outputs
            println!("::starthub:state::{}", json!({
                "status": status,
                "body": body_str
            }).to_string());

            eprintln!("GET {} -> {}", url, status);
        }
        Err(e) => eprintln!("Request error: {}", e),
    }
}
"#;

pub fn readme_tpl(name: &str, kind: &crate::models::ShKind, repo: &str, license: &str) -> String {
    let kind_str = match kind { 
        crate::models::ShKind::Docker => "docker", 
        crate::models::ShKind::Wasm => "wasm",
        crate::models::ShKind::Composition => "composite"
    };
    
    let build_instructions = match kind {
        crate::models::ShKind::Wasm => format!(r#"## Build

Build the WASM module:

```bash
cargo build --release
```

The compiled WASM file will be located at `target/release/{}.wasm`.
"#, name),
        crate::models::ShKind::Docker => format!(r#"## Build

Build the Docker image:

```bash
docker build -t {} .
```
"#, name),
        crate::models::ShKind::Composition => r#"## Build

Composition actions don't require building. They are defined in `starthub.json` and can be published directly.
"#.to_string(),
    };
    
    format!(r#"# {name}

A Starthub **{kind_str}** action.

- Repository: `{repo}`
- License: `{license}`

{build_instructions}
## Usage

... write usage instructions here ...
"#,
        name = name,
        kind_str = kind_str,
        repo = repo,
        license = license,
        build_instructions = build_instructions
    )
}

pub fn wasm_cargo_toml_tpl(name: &str, version: &str) -> String {
    format!(r#"[package]
name = "{name}"
version = "{version}"
edition = "2021"
rust-version = "1.82"
publish = false

[dependencies]
waki = {{ version = "0.4.2", features = ["json", "multipart"] }}
serde = {{ version = "1.0.202", features = ["derive"] }}
serde_json = "1.0"

# reduce wasm binary size
[profile.release]
lto = true
strip = "symbols"
"#)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ShKind;

    #[test]
    fn test_dockerfile_template_contains_expected_content() {
        assert!(DOCKERFILE_TPL.contains("FROM alpine:3.20"));
        assert!(DOCKERFILE_TPL.contains("RUN apk add --no-cache curl jq"));
        assert!(DOCKERFILE_TPL.contains("WORKDIR /app"));
        assert!(DOCKERFILE_TPL.contains("COPY entrypoint.sh /app/entrypoint.sh"));
        assert!(DOCKERFILE_TPL.contains("CMD [\"/app/entrypoint.sh\"]"));
    }

    #[test]
    fn test_entrypoint_template_contains_expected_content() {
        assert!(ENTRYPOINT_SH_TPL.contains("#!/bin/sh"));
        assert!(ENTRYPOINT_SH_TPL.contains("set -euo pipefail"));
        assert!(ENTRYPOINT_SH_TPL.contains("INPUT=\"$(cat || true)\""));
        assert!(ENTRYPOINT_SH_TPL.contains("do_access_token"));
        assert!(ENTRYPOINT_SH_TPL.contains("do_project_id"));
        assert!(ENTRYPOINT_SH_TPL.contains("do_tag_name"));
        assert!(ENTRYPOINT_SH_TPL.contains("::starthub:state::"));
    }

    #[test]
    fn test_gitignore_template_contains_expected_content() {
        assert!(GITIGNORE_TPL.contains("/target"));
        assert!(GITIGNORE_TPL.contains("/dist"));
        assert!(GITIGNORE_TPL.contains("/node_modules"));
        assert!(GITIGNORE_TPL.contains("*.log"));
        assert!(GITIGNORE_TPL.contains("starthub.lock.json"));
    }

    #[test]
    fn test_dockerignore_template_contains_expected_content() {
        assert!(DOCKERIGNORE_TPL.contains("*"));
        assert!(DOCKERIGNORE_TPL.contains("!entrypoint.sh"));
        assert!(DOCKERIGNORE_TPL.contains("!starthub.json"));
        assert!(DOCKERIGNORE_TPL.contains("!Dockerfile"));
    }

    #[test]
    fn test_wasm_main_rs_template_contains_expected_content() {
        assert!(WASM_MAIN_RS_TPL.contains("use waki::Client"));
        assert!(WASM_MAIN_RS_TPL.contains("struct Input"));
        assert!(WASM_MAIN_RS_TPL.contains("fn main()"));
        assert!(WASM_MAIN_RS_TPL.contains("::starthub:state::"));
        assert!(WASM_MAIN_RS_TPL.contains("Client::new()"));
    }

    #[test]
    fn test_readme_template_for_wasm() {
        let name = "test-wasm-package";
        let kind = ShKind::Wasm;
        let repo = "github.com/test/package";
        let license = "MIT";

        let readme = readme_tpl(name, &kind, repo, license);

        assert!(readme.contains(name));
        assert!(readme.contains("wasm"));
        assert!(readme.contains(repo));
        assert!(readme.contains(license));
        assert!(readme.contains("::starthub:state::"));
        assert!(!readme.contains("docker"));
    }

    #[test]
    fn test_readme_template_for_docker() {
        let name = "test-docker-package";
        let kind = ShKind::Docker;
        let repo = "github.com/test/package";
        let license = "Apache-2.0";

        let readme = readme_tpl(name, &kind, repo, license);

        assert!(readme.contains(name));
        assert!(readme.contains("docker"));
        assert!(readme.contains(repo));
        assert!(readme.contains(license));
        assert!(readme.contains("::starthub:state::"));
        assert!(!readme.contains("wasm"));
    }

    #[test]
    fn test_wasm_cargo_toml_template() {
        let name = "test-wasm-package";
        let version = "1.2.3";

        let cargo_toml = wasm_cargo_toml_tpl(name, version);

        assert!(cargo_toml.contains(&format!("name = \"{}\"", name)));
        assert!(cargo_toml.contains(&format!("version = \"{}\"", version)));
        assert!(cargo_toml.contains("edition = \"2021\""));
        assert!(cargo_toml.contains("rust-version = \"1.82\""));
        assert!(cargo_toml.contains("waki = { version = \"0.4.2\""));
        assert!(cargo_toml.contains("serde = { version = \"1.0.202\""));
        assert!(cargo_toml.contains("serde_json = \"1.0\""));
        assert!(cargo_toml.contains("lto = true"));
        assert!(cargo_toml.contains("strip = \"symbols\""));
    }

    #[test]
    fn test_template_constants_are_not_empty() {
        assert!(!DOCKERFILE_TPL.is_empty());
        assert!(!ENTRYPOINT_SH_TPL.is_empty());
        assert!(!GITIGNORE_TPL.is_empty());
        assert!(!DOCKERIGNORE_TPL.is_empty());
        assert!(!WASM_MAIN_RS_TPL.is_empty());
    }

    #[test]
    fn test_template_functions_with_different_inputs() {
        // Test with different package names
        let cargo1 = wasm_cargo_toml_tpl("package1", "1.0.0");
        let cargo2 = wasm_cargo_toml_tpl("package2", "2.0.0");

        assert_ne!(cargo1, cargo2);
        assert!(cargo1.contains("package1"));
        assert!(cargo2.contains("package2"));

        // Test with different versions
        let cargo3 = wasm_cargo_toml_tpl("same-package", "1.0.0");
        let cargo4 = wasm_cargo_toml_tpl("same-package", "2.0.0");

        assert_ne!(cargo3, cargo4);
        assert!(cargo3.contains("1.0.0"));
        assert!(cargo4.contains("2.0.0"));
    }
}
