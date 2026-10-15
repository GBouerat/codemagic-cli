//! Configuration management for `codemagic-cli`.
//!
//! Handles loading, saving, and resolving credentials and default settings
//! stored on the local filesystem under the user's OS configuration directory,
//! as well as environment variable and CLI option overrides.

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Project qualifier used by `directories::ProjectDirs` ("io").
const QUALIFIER: &str = "io";

/// Project organization used by `directories::ProjectDirs` ("codemagic").
const ORGANIZATION: &str = "codemagic";

/// Project application name used by `directories::ProjectDirs` ("codemagic-cli").
const APPLICATION: &str = "codemagic-cli";

/// Standard filename for the persistent TOML configuration ("config.toml").
const CONFIG_FILE_NAME: &str = "config.toml";

/// Persistent configuration model saved to `config.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct Config {
    /// Personal Codemagic API authentication token.
    pub api_token: Option<String>,

    /// Default team ID to use when no `--team-id` option is specified.
    pub default_team_id: Option<String>,

    /// Default application ID to use when no `--app-id` option is specified.
    pub default_app_id: Option<String>,

    /// Custom base URL for Codemagic REST API v3 (defaults to `https://codemagic.io`).
    pub v3_base_url: Option<String>,

    /// Custom base URL for Codemagic classic API (defaults to `https://api.codemagic.io`).
    pub classic_base_url: Option<String>,
}

impl Config {
    /// Returns the absolute path to the configuration directory based on operating system conventions.
    ///
    /// - **Linux**: `~/.config/codemagic-cli`
    /// - **macOS**: `~/Library/Application Support/io.codemagic.codemagic-cli`
    /// - **Windows**: `%APPDATA%\codemagic\codemagic-cli\config`
    pub fn config_dir() -> Result<PathBuf> {
        let proj_dirs = ProjectDirs::from(QUALIFIER, ORGANIZATION, APPLICATION)
            .context("Failed to determine user home/config directory")?;
        Ok(proj_dirs.config_dir().to_path_buf())
    }

    /// Returns the absolute path to the `config.toml` file.
    pub fn config_path() -> Result<PathBuf> {
        Ok(Self::config_dir()?.join(CONFIG_FILE_NAME))
    }

    /// Loads the configuration from the default OS configuration path,
    /// returning `Config::default()` if the file does not exist.
    pub fn load() -> Result<Self> {
        let path = Self::config_path()?;
        Self::load_from(&path)
    }

    /// Loads the configuration from a specific file path,
    /// returning `Config::default()` if the file does not exist.
    pub fn load_from(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read config file at {:?}", path))?;
        let config: Config = toml::from_str(&content)
            .with_context(|| format!("Failed to parse config file at {:?}", path))?;
        Ok(config)
    }

    /// Serializes and saves the configuration to the default OS configuration path.
    ///
    /// On Unix platforms, creates and restricts the file with `0600` permissions
    /// (read/write only by file owner) before writing credentials.
    pub fn save(&self) -> Result<()> {
        let path = Self::config_path()?;
        self.save_to(&path)
    }

    /// Serializes and saves the configuration to a specific file path.
    ///
    /// On Unix platforms, atomically creates the file with mode `0600` and enforces
    /// `0600` permissions on existing files before writing any secret token data.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent()
            && !dir.as_os_str().is_empty()
            && !dir.exists()
        {
            fs::create_dir_all(dir)
                .with_context(|| format!("Failed to create config directory at {:?}", dir))?;
        }

        let content =
            toml::to_string_pretty(self).context("Failed to serialize configuration to TOML")?;

        #[cfg(unix)]
        {
            use std::io::Write;
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

            let mut file = fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .mode(0o600)
                .open(path)
                .with_context(|| format!("Failed to open config file at {:?}", path))?;

            let mut permissions = file
                .metadata()
                .with_context(|| format!("Failed to read metadata for config file at {:?}", path))?
                .permissions();
            permissions.set_mode(0o600);
            fs::set_permissions(path, permissions).with_context(|| {
                format!(
                    "Failed to set 0600 permissions on config file at {:?}",
                    path
                )
            })?;

            file.write_all(content.as_bytes())
                .with_context(|| format!("Failed to write config file to {:?}", path))?;
            file.flush()
                .with_context(|| format!("Failed to flush config file at {:?}", path))?;
        }

        #[cfg(not(unix))]
        {
            fs::write(path, content)
                .with_context(|| format!("Failed to write config file to {:?}", path))?;
        }

        Ok(())
    }

    /// Clears the stored API token from the configuration and saves changes to disk.
    pub fn clear_token(&mut self) -> Result<()> {
        self.api_token = None;
        self.save()
    }

    /// Resolves the Codemagic API token using a 3-tier precedence hierarchy:
    ///
    /// 1. Explicit command-line argument (`--token <TOKEN>`)
    /// 2. `CODEMAGIC_API_TOKEN` environment variable
    /// 3. Persistent `config.toml` file
    ///
    /// Returns `None` if no valid non-empty token is found.
    pub fn resolve_token(cli_token: Option<&str>, config: &Config) -> Option<String> {
        if let Some(token) = cli_token
            && !token.trim().is_empty()
        {
            return Some(token.trim().to_string());
        }
        if let Ok(token) = std::env::var("CODEMAGIC_API_TOKEN")
            && !token.trim().is_empty()
        {
            return Some(token.trim().to_string());
        }
        config
            .api_token
            .as_ref()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
    }
}
