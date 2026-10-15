//! Raw authenticated API request CLI command.
//!
//! Provides the `codemagic api <METHOD> <PATH> [-f FILE|-]` subcommand for raw authenticated
//! HTTP calls against any Codemagic endpoint.

use anyhow::{Context, Result};
use clap::Args;
use std::io::Read;

use crate::client::CodemagicClient;
use crate::output::OutputFormat;

/// CLI arguments container for the raw API subcommand.
#[derive(Debug, Args)]
pub struct ApiArgs {
    /// HTTP method (e.g. GET, POST, PUT, DELETE, PATCH)
    pub method: String,

    /// API endpoint path relative to API root (e.g. `/builds/123`), or a full http(s) URL
    pub path: String,

    /// Path to a file containing JSON request body, or '-' to read from standard input
    #[arg(short = 'f', long = "file")]
    pub file: Option<String>,
}

/// Dispatches execution of the raw API subcommand.
pub async fn handle_api(
    args: ApiArgs,
    client: &CodemagicClient,
    _output_format: OutputFormat,
) -> Result<()> {
    let method_upper = args.method.to_uppercase();
    let method: reqwest::Method = method_upper
        .parse()
        .with_context(|| format!("Invalid HTTP method '{}'", args.method))?;

    let url = if args.path.starts_with("http://") || args.path.starts_with("https://") {
        args.path.clone()
    } else {
        let base = client.classic_base().trim_end_matches('/');
        let path = if args.path.starts_with('/') {
            &args.path
        } else {
            &format!("/{}", args.path)
        };
        format!("{}{}", base, path)
    };

    let payload: Option<serde_json::Value> = if let Some(ref file_path) = args.file {
        let raw_json = if file_path == "-" {
            let mut stdin_buf = String::new();
            std::io::stdin()
                .read_to_string(&mut stdin_buf)
                .context("Failed to read JSON payload from stdin")?;
            stdin_buf
        } else {
            std::fs::read_to_string(file_path)
                .with_context(|| format!("Failed to read JSON payload from file '{}'", file_path))?
        };

        let trimmed = raw_json.trim();
        if trimmed.is_empty() {
            None
        } else {
            let val = serde_json::from_str(trimmed)
                .with_context(|| format!("Invalid JSON payload in '{}'", file_path))?;
            Some(val)
        }
    } else {
        None
    };

    let response_text = client
        .raw_request(method, &url, payload.as_ref())
        .await
        .map_err(|e| {
            let clean_url = url.split('?').next().unwrap_or(&url);
            match e {
                crate::client::error::CodemagicError::ApiError { status, message } => {
                    anyhow::anyhow!(
                        "{} {} -> HTTP {}: {}",
                        method_upper,
                        clean_url,
                        status,
                        message
                    )
                }
                _ => anyhow::anyhow!("{} {} -> {}", method_upper, clean_url, e),
            }
        })?;

    let trimmed = response_text.trim();
    if trimmed.is_empty() {
        println!("null");
    } else if let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) {
        println!("{}", serde_json::to_string_pretty(&val)?);
    } else {
        println!("{}", trimmed);
    }

    Ok(())
}
