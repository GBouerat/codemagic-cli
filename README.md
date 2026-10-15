# codemagic-cli

A fast, ergonomic, cross-platform CLI tool for the [Codemagic](https://codemagic.io) CI/CD service, written in Rust.

[![Crates.io](https://img.shields.io/crates/v/codemagic.svg)](https://crates.io/crates/codemagic)
[![docs.rs](https://docs.rs/codemagic/badge.svg)](https://docs.rs/codemagic)
[![CI](https://github.com/GBouerat/codemagic-cli/actions/workflows/ci.yml/badge.svg)](https://github.com/GBouerat/codemagic-cli/actions/workflows/ci.yml)
[![GitHub Release](https://img.shields.io/github/v/release/GBouerat/codemagic-cli)](https://github.com/GBouerat/codemagic-cli/releases)
![License](https://img.shields.io/badge/license-Apache%202.0-blue.svg)

---

## Features

- **Authentication Management**: Securely store and validate your personal Codemagic API token or override via environment variables/CLI options.
- **Applications (`app`)**:
  - List applications (with team filtering and search).
  - Inspect application details, branch lists, and workflow definitions.
  - Register new Git repositories as Codemagic apps.
- **Builds (`build`)**:
  - List recent builds with colored status badges (defaults to current Git branch; use `--branch ''` for all branches).
  - Inspect full build details, commit info, artifacts, and numbered build action steps.
  - Inspect build step logs as clean plain text (`build log`) with automatic ANSI/HTML sanitization and tailing.
  - Trigger new builds with optional app ID (falls back to configured default app) and automatic Git branch detection.
  - Cancel running or queued builds.
  - **Live Watch Mode**: Follow build progress with real-time terminal spinners until completion.
- **Artifacts (`artifact`)**:
  - List generated artifacts (IPAs, APKs, AABs, logs, etc.) for a build.
  - Download artifacts with substring matching across multiple `-n/--name` filters and progress bars.
  - Generate expiring public download URLs for easy sharing.
- **Raw API (`api`)**:
  - Execute arbitrary authenticated API calls with formatted JSON responses and payload support (`-f FILE|-`).
- **Environment Variables (`env`)**:
  - Inspect variable groups and environment variable configurations.
- **Caches (`cache`)**:
  - List stored dependency and build caches across workflows and branches.
  - Delete individual caches or purge all caches for an application.
- **Flexible Output**:
  - Default human-friendly tables with colors.
  - Structured `--output json` mode for scripting and CI/CD pipelines (e.g. `jq`).

---

## Installation

### Homebrew (macOS & Linux)

```bash
brew install GBouerat/tap/codemagic
```

### Cargo (crates.io)

```bash
cargo install codemagic
```

### Pre-built Binaries

Pre-compiled standalone binaries for Linux (`x86_64`, `aarch64` musl), macOS (Apple Silicon & Intel), and Windows (`x86_64`) are automatically published for each version on the [Releases](https://github.com/GBouerat/codemagic-cli/releases) page.

### From Source

Ensure you have Rust and Cargo installed (1.80+):

```bash
git clone https://github.com/GBouerat/codemagic-cli.git
cd codemagic-cli
cargo build --release
```

The compiled binary will be located at `target/release/codemagic`.

You can install it to your Cargo bin directory:

```bash
cargo install --path .
```


---

## Configuration & Authentication

### 1. Interactive Login

```bash
codemagic auth login
```

This prompts for your **Codemagic API token** (found under **Codemagic > Account settings > API token**), verifies it against the API, and stores it in your user configuration file (`~/.config/codemagic-cli/config.toml` on Linux, `~/Library/Application Support/io.codemagic.codemagic-cli/config.toml` on macOS, or `%APPDATA%\codemagic\codemagic-cli\config\config.toml` on Windows) with restricted `0600` permissions on Unix.

### 2. Environment Variable

You can also authenticate without storing credentials by setting `CODEMAGIC_API_TOKEN`:

```bash
export CODEMAGIC_API_TOKEN="your-api-token"
```

### 3. CLI Override

All commands accept `--token <TOKEN>` to override stored credentials:

```bash
codemagic --token "your-api-token" app list
```

---

## Usage Guide

### Authentication Commands

```bash
# Check current authentication status and user ID
codemagic auth status

# Log out and remove stored token
codemagic auth logout
```

### Applications (`app`)

```bash
# List personal apps
codemagic app list

# List apps for a specific team
codemagic app list --team-id <TEAM_ID>

# Search apps by name
codemagic app list --name "Flutter"

# Get detailed info (branches and workflows) for an application
codemagic app get <APP_ID>

# List workflows defined for an application
codemagic app workflows <APP_ID>

# Register a new repository
codemagic app add git@github.com:my-org/my-app.git
```

### Builds (`build`)

```bash
# List recent builds for a team (defaults to current Git branch)
codemagic build list --team-id <TEAM_ID>

# List recent builds for all branches
codemagic build list --branch ''

# Get details of a specific build (including commit, artifacts, and numbered steps)
codemagic build get <BUILD_ID>

# Print build step logs as plain text (defaults to first failed/timeout/canceled step)
codemagic build log <BUILD_ID>

# Print step log by 1-based step number or substring of step name, with tail limit
codemagic build log <BUILD_ID> 3 --tail 100
codemagic build log <BUILD_ID> "flutter build" --tail 50

# Trigger a build using configured default app and current Git branch
codemagic build start --workflow "release"

# Trigger a build specifying app ID and branch
codemagic build start <APP_ID> --workflow "release" --branch "main"

# Trigger a build on a tag with custom machine and wait for completion
codemagic build start <APP_ID> \
  --workflow "release" \
  --tag "v1.0.0" \
  --machine "mac_mini_m2" \
  --wait

# Trigger a build passing custom variables
codemagic build start <APP_ID> \
  --workflow "staging" \
  --branch "develop" \
  --var "API_URL=https://api.staging.example.com" \
  --var "DEBUG=true"

# Live-watch an already running build
codemagic build watch <BUILD_ID>

# Cancel a build
codemagic build cancel <BUILD_ID>
```

### Artifacts (`artifact`)

```bash
# List all artifacts generated by a build
codemagic artifact list <BUILD_ID>

# Download all artifacts to the current directory
codemagic artifact download <BUILD_ID>

# Download artifacts matching substring filters (can be passed multiple times)
codemagic artifact download <BUILD_ID> -n "apk" -n "mapping" --dest ./dist

# Create an expiring public share link (valid for 24 hours by default)
codemagic artifact share <BUILD_ID> --name "app-release.ipa" --expires-in 86400
```

### Raw API Calls (`api`)

For any Codemagic REST endpoint, perform raw authenticated calls with pretty JSON formatting:

```bash
# GET an endpoint relative to API root
codemagic api GET /builds/<BUILD_ID>

# POST JSON payload from a file
codemagic api POST /builds -f payload.json

# POST JSON payload from stdin
echo '{"workflowId": "release"}' | codemagic api POST /builds -f -

# Full URL support
codemagic api GET https://api.codemagic.io/apps
```

### Environment Variables (`env`)

```bash
# List variable groups configured for an application (falls back to default app)
codemagic env groups --app-id <APP_ID>

# List variables inside a variable group
codemagic env list <GROUP_ID>
```

### Caches (`cache`)

```bash
# List all stored build and dependency caches for an app
codemagic cache list --app-id <APP_ID>

# Delete a specific cache by ID
codemagic cache delete --app-id <APP_ID> --cache-id <CACHE_ID>

# Purge all caches for an app (skip interactive confirmation with -y)
codemagic cache delete --app-id <APP_ID> --yes
```

### JSON Output for Scripting

Add `-o json` to any command for clean, scriptable JSON output:

```bash
# Get the latest build ID for an app via jq
codemagic -o json build list --team-id <TEAM_ID> | jq -r '.data[0].id'
```

---

## Shell Auto-Completion

`codemagic` can generate auto-completion scripts for **Bash**, **Zsh**, and **Fish**.

### Zsh

```zsh
mkdir -p ~/.zfunc
codemagic completions zsh > ~/.zfunc/_codemagic
```

Ensure `~/.zfunc` is included in your `$fpath` in `~/.zshrc` (before calling `compinit`):

```zsh
fpath=(~/.zfunc $fpath)
autoload -Uz compinit && compinit
```

### Bash

```bash
# On modern Linux distributions:
codemagic completions bash > ~/.local/share/bash-completion/completions/codemagic

# Or load directly in ~/.bashrc:
eval "$(codemagic completions bash)"
```

### Fish

```fish
mkdir -p ~/.config/fish/completions
codemagic completions fish > ~/.config/fish/completions/codemagic.fish
```

---

## Architecture

For an in-depth breakdown of the internal design, multi-tier API client architecture, resilient data deserialization models, and testing strategies, see [ARCHITECTURE.md](ARCHITECTURE.md).

---

## License

Licensed under the Apache License, Version 2.0 or MIT license ([LICENSE](LICENSE)).

---

*Generated entirely by Gemini through Antigravity.*

