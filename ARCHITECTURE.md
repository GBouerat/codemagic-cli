# Technical Architecture & Internal Design

This document details the internal design, asynchronous dispatch pipeline, REST client architecture, data modeling, and engineering decisions behind `codemagic`.

---

## 1. System Architecture Overview

```
                      ┌─────────────────────────────────────────┐
                      │               src/main.rs               │
                      │  - CLI parsing (clap)                   │
                      │  - Config loading & token resolution    │
                      │  - Async runtime setup (tokio)          │
                      │  - Top-level error formatting           │
                      └────────────────────┬────────────────────┘
                                           │
         ┌───────────────────┬─────────────┼───────────────┬───────────────────┐
         ▼                   ▼             ▼               ▼                   ▼
  ┌──────────────┐   ┌──────────────┐┌──────────────┐┌──────────────┐   ┌──────────────┐
  │src/cli/auth  │   │ src/cli/app  ││src/cli/build ││src/cli/art.. │   │ src/cli/env  │
  │- Login/logout│   │- List / get  ││- List / get  ││- List        │   │- Groups      │
  │- Status check│   │- Workflows   ││- Start/cancel││- Download bar│   │- Variables   │
  │- Secure store│   │- Add repo    ││- Watch loop  ││- Public URLs │   │              │
  └──────┬───────┘   └──────┬───────┘└──────┬───────┘└──────┬───────┘   └──────┬───────┘
         │                  │               │               │                  │
         └──────────────────┼───────────────┼───────────────┴──────────────────┘
                            ▼               ▼
             ┌──────────────────────────────────────────────┐
             │             src/client/mod.rs                │
             │  - Dual-tier client (v3 REST & classic API)  │
             │  - Rustls TLS (zero dynamic C dependencies)  │
             │  - x-auth-token header injection             │
             │  - Redirect-aware presigned S3/GCS downloads │
             │  - Error mapping & status verification       │
             └──────────────────────┬───────────────────────┘
                                    │
                    ┌───────────────┴───────────────┐
                    ▼                               ▼
     ┌─────────────────────────────┐ ┌─────────────────────────────┐
     │      src/models/mod.rs      │ │      src/output/mod.rs      │
     │  - Strongly typed Serde     │ │  - Comfy-table formatters   │
     │  - Resilient deserializers  │ │  - Colored status badges    │
     │  - Polymorphic release_notes│ │  - ANSI-safe cell styling   │
     │  - Cursor pagination models │ │  - Raw JSON streaming mode  │
     └─────────────────────────────┘ └─────────────────────────────┘
```

The system is designed around a **clean separation between I/O, network communication, and data presentation**:
- **`CodemagicClient`** in [`src/client/mod.rs`](src/client/mod.rs) encapsulates all HTTP communication, header construction, dual-base URL routing, redirect-safe artifact downloading, and error normalization.
- **`models`** in [`src/models/mod.rs`](src/models/mod.rs) provide resilient, schema-tolerant Serde structures that accommodate variations between Codemagic API versions without crashing.
- **`output`** in [`src/output/mod.rs`](src/output/mod.rs) decouples data presentation from business logic, ensuring both rich terminal rendering and machine-readable JSON formatting.
- **`cli`** submodules in [`src/cli/`](src/cli/) encapsulate isolated subcommand logic, providing clean separation and minimal cross-module coupling.

---

## 2. Core Components Deep-Dive

### 2.1 Multi-Tier REST API Client (`src/client/mod.rs`)

Codemagic's backend infrastructure exposes two distinct API endpoints with different path conventions and response envelopes:

| API Tier | Base URL | Endpoints Handled |
| :--- | :--- | :--- |
| **REST API v3** | `https://codemagic.io` | `/api/v3/user`, `/api/v3/user/teams`, `/api/v3/user/apps`, `/api/v3/teams/{id}/apps`, `/api/v3/teams/{id}/builds`, `/api/v3/builds/{id}`, `/api/v3/apps/{id}/builds`, `/api/v3/apps/{id}/workflows`, `/api/v3/apps/{id}/variable-groups`, `/api/v3/variable-groups/{id}/variables` |
| **Classic API** | `https://api.codemagic.io` | `/apps/:id` (legacy branch discovery), `POST /apps` (repo registration), `POST /builds/{id}/cancel`, `POST /artifacts/{path}/public-url` |

`CodemagicClient` routes requests seamlessly across both tiers:

```rust
pub struct CodemagicClient {
    client: reqwest::Client,
    no_redirect_client: reqwest::Client,
    download_client: reqwest::Client,
    api_token: String,
    v3_base: String,      // Defaults to https://codemagic.io
    classic_base: String, // Defaults to https://api.codemagic.io
}
```

#### Authentication & Redirect-Safe Artifact Downloads
Unlike OAuth2 or standard Bearer authentication (`Authorization: Bearer <token>`), Codemagic's API expects the token passed in the custom header `x-auth-token`.
- Standard API calls use `client` (which attaches `x-auth-token` by default).
- When downloading artifacts in `download_file`:
  - If the URL targets a Codemagic host (`v3_base` or `classic_base`), the initial request uses `no_redirect_client` (which sends `x-auth-token` with automatic redirect following disabled).
  - If Codemagic responds with a `3xx` redirect to an external cloud storage provider (e.g. AWS S3 or Google Cloud Storage presigned URLs), the redirect target is fetched using `download_client` (which has no `x-auth-token` header), preventing credential leakage and S3 `SignatureDoesNotMatch` errors.

#### Static Rustls Networking
The client uses `reqwest` with `rustls-tls` enabled and default features disabled (`default-features = false`). This eliminates any dependency on system OpenSSL libraries (`libssl.so` or `libcrypto.so`), enabling statically compiled binaries that run seamlessly across any Linux distribution, container, or Alpine/musl environment without runtime linking errors.

#### Cursor-Based Pagination
Codemagic REST v3 endpoints utilize cursor-based pagination. The client abstracts parameter encoding (`?cursor=...&page_size=20`), allowing callers to fetch paginated collections deterministically.

---

### 2.2 Hierarchical Configuration & Token Resolution (`src/config/mod.rs`)

`Config::resolve_token` resolves credentials through a strict 3-tier precedence hierarchy:

```
┌──────────────────────────────────────────────┐
│  1. CLI Flag: --token <TOKEN>                │  (Highest priority)
└──────────────────────┬───────────────────────┘
                       ▼
┌──────────────────────────────────────────────┐
│  2. Environment: CODEMAGIC_API_TOKEN         │  (CI/CD and shell workflows)
└──────────────────────┬───────────────────────┘
                       ▼
┌──────────────────────────────────────────────┐
│  3. File: config.toml                        │  (Persistent user login)
└──────────────────────────────────────────────┘
```

Configuration files are stored using `directories::ProjectDirs::from("io", "codemagic", "codemagic-cli")`:
- **Linux**: `~/.config/codemagic-cli/config.toml`
- **macOS**: `~/Library/Application Support/io.codemagic.codemagic-cli/config.toml`
- **Windows**: `%APPDATA%\codemagic\codemagic-cli\config\config.toml`

On Unix systems, `Config::save_to` uses `OpenOptionsExt::mode(0o600)` and `PermissionsExt::set_mode(0o600)` to ensure the configuration file is created and maintained with `0600` (owner read/write only) permissions before writing any token data.

---

### 2.3 Command Dispatch & Async Subcommand Handlers (`src/cli/`)

The CLI structure is organized into dedicated modules:

- **`cli::auth`** ([`src/cli/auth.rs`](src/cli/auth.rs)):
  - Interactive login via `dialoguer::Password` without terminal echo.
  - Proactive validation: tests the token against `GET /api/v3/user` before persisting to disk.
  - Status inspector showing authentication state, user ID, default team ID, and accessible teams.
- **`cli::app`** ([`src/cli/app.rs`](src/cli/app.rs)):
  - Team-filtered and name-filtered application listing.
  - Deep application inspection: branches and workflow definitions.
  - Repository registration (`codemagic app add <repo_url>`).
- **`cli::build`** ([`src/cli/build.rs`](src/cli/build.rs)):
  - Comprehensive build trigger: branch, tag, workflow name, machine type, custom environment variables (`KEY=VALUE`), labels, and variable groups.
  - Build cancellation (handling HTTP `200` and HTTP `208 Already Reported`).
  - Real-time watch and progress streaming.
- **`cli::artifact`** ([`src/cli/artifact.rs`](src/cli/artifact.rs)):
  - Artifact listing with human-readable file sizes and types.
  - Path-traversal-safe filename sanitization (`sanitize_artifact_filename`).
  - Streaming file downloader with byte-level progress bar.
  - Generation of expiring public share links (`POST /artifacts/{path}/public-url`).
- **`cli::env`** ([`src/cli/env.rs`](src/cli/env.rs)):
  - Inspection of variable groups and environment variable configurations.
- **`cli::completions`** ([`src/cli/mod.rs`](src/cli/mod.rs)):
  - Native shell completion generator powered by `clap_complete`.

---

### 2.4 Streaming Progress & Polling Engine (`src/cli/build.rs`)

When `--wait` or `codemagic build watch <build_id>` is invoked, the CLI enters an asynchronous polling loop:

1. Polls `GET /api/v3/builds/{build_id}` every `interval` seconds (default: 3 seconds, minimum: 1 second).
2. Tolerates up to 5 consecutive transient network errors before aborting.
3. Displays an animated `indicatif` spinner in table mode (hidden automatically in `--output json` mode).
4. On terminal state (`finished`, `failed`, `errored`, `timeout`, `canceled`):
   - Clears spinner and emits final build details.
   - **Exit code propagation**: Exits with code `0` on `finished`/`success`, or code `1` on `failed`/`errored`/`timeout`/`canceled`, enabling reliable CI/CD script orchestration.

---

### 2.5 Resilient Data Modeling & Schema Normalization (`src/models/mod.rs`)

Codemagic API responses exhibit variations across v3 and classic endpoints. [`src/models/build.rs`](src/models/build.rs) implements resilient Serde deserialization to guarantee zero-panic parsing:

#### Polymorphic Release Notes Deserialization
In the Codemagic v3 API, `release_notes` can arrive as either:
- A plain JSON string: `"release_notes": "Bug fixes and performance improvements"`
- An array of localized objects:
  ```json
  "release_notes": [
    { "language": "en", "text": "Bug fixes and performance improvements" }
  ]
  ```

`models::Build` stores `release_notes: Option<serde_json::Value>` and exposes a unified `.release_notes_summary()` helper that normalizes both representations into a clean string.

#### Cursor Pagination Normalization
`CursorPagination<T>` handles responses structured with `data: Vec<T>`, `page_size: Option<u32>`, and polymorphic `cursor` representations (raw string, MongoDB `{"$oid": "..."}` object, or `null`).

---

### 2.6 Dual Rendering Modes (`src/output/mod.rs`)

#### 1. Terminal Tables with Boundary-Safe Cell Styling
When emitting formatted tables via `comfy-table`:
- **Problem**: Embedding raw ANSI escape strings inside table cells causes character count mismatches, corrupting box-drawing borders and alignment.
- **Solution**: Styling is applied directly to table cells using `comfy_table::Cell::fg(Color)` rather than raw ANSI string escapes. This guarantees pixel-perfect table borders across all terminal emulators.

#### 2. Machine-Readable JSON Mode
When `--output json` (or `-o json`) is supplied, data models are serialized directly to stdout as formatted JSON. All informational logs, spinners, and diagnostics are routed to stderr, ensuring clean `jq` pipeability:

```bash
codemagic -o json build list --team-id "$TEAM_ID" | jq -r '.data[0].id'
```

---

### 2.7 Shell Auto-Completions (`clap_complete`)

The `codemagic completions <shell>` command utilizes `clap_complete` to dynamically inspect the command tree and generate high-performance completion scripts for:
- **Bash** (`~/.bashrc` / `~/.bash_completion`)
- **Zsh** (`~/.zfunc/_codemagic`)
- **Fish** (`~/.config/fish/completions/codemagic.fish`)
- **PowerShell** & **Elvish**

The command tree is structured to avoid Clap short option or global argument ID collisions (e.g. reserving `-o`/`--output` globally for output format and `-d`/`--dest` for artifact download paths), preventing runtime panics during completion generation and argument downcasting.

---

## 3. Testing Strategy

The test suite in [`tests/cli_tests.rs`](tests/cli_tests.rs) provides exhaustive regression safety across multiple tiers:

1. **Token Resolution & Config Persistence**:
   - Thread-safe testing of the 3-tier token resolution precedence hierarchy.
   - Config TOML serialization round-trips and Unix `0600` file permission verification.
2. **Input Sanitization & Validation**:
   - Path traversal rejection and nested path extraction in `sanitize_artifact_filename`.
   - `KEY=VALUE` parsing and empty-key rejection in `parse_env_vars`.
3. **Output Formatters & Units**:
   - Byte size formatting (`format_size`) across bytes, KB, MB, and GB boundaries.
   - ANSI color formatting and cell color mapping for build statuses.
   - Non-ASCII UTF-8 character boundary safety in table rendering and error snippets.
4. **Payload Deserialization against Real Codemagic Payloads**:
   - v3 application list, workflow definitions, build objects with polymorphic `release_notes`, and cursor pagination formats.
5. **Local HTTP Mock Server Integration Tests (`CodemagicClient`)**:
   - Exercises `CodemagicClient` endpoints against an asynchronous local TCP mock server, verifying `x-auth-token` headers, HTTP status error mapping (`401`, `403`, `404`, `429`, `500`), HTTP `208` build cancellation handling, and redirect-safe artifact downloading without leaking auth headers to external storage hosts.
6. **Shell Auto-Completion Generation**:
   - Generates completion scripts across all 5 supported shells (Bash, Zsh, Fish, PowerShell, Elvish).

---

## 4. Multi-Platform Automated Release Pipeline

### Continuous Integration (`.github/workflows/ci.yml`)
- Runs on every push and pull request against `dev`, `main`, and `master`.
- Matrix test execution across `ubuntu-latest` and `macos-latest`.
- Enforces strict code formatting (`cargo fmt --check`), lint hygiene (`cargo clippy --all-targets -- -D warnings`), and test execution (`cargo test --verbose`).
- Utilizes `Swatinem/rust-cache@v2` for rapid compilation caching.

### Binary Release Automation (`.github/workflows/release.yml`)
- Automatically triggered upon pushing version tags (`v*.*.*`).
- Matrix compilation across 5 target platforms:
  - **`x86_64-unknown-linux-musl`**: Statically linked for any 64-bit Linux distribution.
  - **`aarch64-unknown-linux-musl`**: Statically linked for ARM64 Linux (Raspberry Pi, AWS Graviton, etc.).
  - **`aarch64-apple-darwin`**: Native Apple Silicon binary (M1/M2/M3/M4).
  - **`x86_64-apple-darwin`**: Intel macOS binary.
  - **`x86_64-pc-windows-msvc`**: 64-bit Windows executable.
- Packages compressed `.tar.gz` and `.zip` archives with README and LICENSE.
- Generates individual `.sha256` checksums and a unified `SHA256SUMS.txt` manifest.
- Publishes assets automatically to GitHub Releases using `softprops/action-gh-release@v2`.
