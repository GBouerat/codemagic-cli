//! Application storage and dependency cache CLI commands.
//!
//! Provides subcommands to inspect stored build caches for an application
//! and purge or delete individual or all caches.

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use colored::Colorize;
use serde::{Deserialize, Serialize};

use crate::client::CodemagicClient;
use crate::config::Config;
use crate::output::{OutputFormat, print_output, render_caches_table};

/// CLI arguments container for cache management subcommands.
#[derive(Debug, Args)]
pub struct CacheArgs {
    /// Cache subcommand to execute.
    #[command(subcommand)]
    pub command: CacheCommands,
}

/// Subcommands for listing and purging application storage caches.
#[derive(Debug, Subcommand)]
pub enum CacheCommands {
    /// List stored application caches
    List {
        /// Application ID (falls back to default_app_id in config if omitted)
        #[arg(long, short = 'a')]
        app_id: Option<String>,
    },
    /// Delete application caches
    Delete {
        /// Application ID (falls back to default_app_id in config if omitted)
        #[arg(long, short = 'a')]
        app_id: Option<String>,

        /// Specific cache ID to delete (if omitted, prompts to delete all caches)
        #[arg(long, short = 'c')]
        cache_id: Option<String>,

        /// Skip interactive confirmation prompt
        #[arg(long, short = 'y')]
        yes: bool,
    },
}

/// Structured output for cache deletion operations.
#[derive(Debug, Serialize, Deserialize)]
pub struct DeleteCacheOutput {
    /// Application ID whose cache was deleted.
    pub app_id: String,
    /// Deleted cache ID, or `None` if all caches were purged.
    pub cache_id: Option<String>,
    /// Whether the deletion succeeded.
    pub success: bool,
    /// Human-readable outcome message.
    pub message: String,
}

/// Dispatches execution of cache subcommands.
pub async fn handle_cache(
    command: CacheCommands,
    client: &CodemagicClient,
    config: &Config,
    output_format: OutputFormat,
) -> Result<()> {
    match command {
        CacheCommands::List { app_id } => {
            let app_id = app_id
                .or_else(|| config.default_app_id.clone())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "Application ID required. Please pass '--app-id <ID>' or set a default app in config."
                    )
                })?;

            let caches = client
                .list_caches(&app_id)
                .await
                .with_context(|| format!("Failed to list caches for app '{}'", app_id))?;

            print_output(output_format, &caches, |c| {
                render_caches_table(c, &app_id);
            });
        }
        CacheCommands::Delete {
            app_id,
            cache_id,
            yes,
        } => {
            let app_id = app_id
                .or_else(|| config.default_app_id.clone())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "Application ID required. Please pass '--app-id <ID>' or set a default app in config."
                    )
                })?;

            if let Some(ref cid) = cache_id {
                client.delete_cache(&app_id, cid).await.with_context(|| {
                    format!("Failed to delete cache '{}' for app '{}'", cid, app_id)
                })?;

                let message = format!("Cache '{}' deleted successfully.", cid);
                let out = DeleteCacheOutput {
                    app_id,
                    cache_id: Some(cid.clone()),
                    success: true,
                    message: message.clone(),
                };

                match output_format {
                    OutputFormat::Json => {
                        println!("{}", serde_json::to_string_pretty(&out)?);
                    }
                    OutputFormat::Table => {
                        println!("{}", message.green().bold());
                    }
                }
            } else {
                if !yes && output_format != OutputFormat::Json {
                    let prompt = format!(
                        "Are you sure you want to delete ALL caches for app '{}'?",
                        app_id
                    );
                    let confirmed = dialoguer::Confirm::new()
                        .with_prompt(prompt)
                        .default(false)
                        .interact()
                        .unwrap_or(false);

                    if !confirmed {
                        println!("{}", "Operation cancelled.".yellow());
                        return Ok(());
                    }
                }

                client
                    .delete_all_caches(&app_id)
                    .await
                    .with_context(|| format!("Failed to delete all caches for app '{}'", app_id))?;

                let message = format!("All caches for app '{}' deleted successfully.", app_id);
                let out = DeleteCacheOutput {
                    app_id,
                    cache_id: None,
                    success: true,
                    message: message.clone(),
                };

                match output_format {
                    OutputFormat::Json => {
                        println!("{}", serde_json::to_string_pretty(&out)?);
                    }
                    OutputFormat::Table => {
                        println!("{}", message.green().bold());
                    }
                }
            }
        }
    }

    Ok(())
}
