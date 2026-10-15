//! Application management CLI commands.
//!
//! Provides subcommands to list, inspect, register, and query workflows
//! for Codemagic applications.

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use colored::Colorize;

use crate::client::CodemagicClient;
use crate::config::Config;
use crate::models::AddAppRequest;
use crate::output::{OutputFormat, print_output, render_apps_table, render_workflows_table};

/// CLI arguments container for application subcommands.
#[derive(Debug, Args)]
pub struct AppArgs {
    /// App subcommand to execute.
    #[command(subcommand)]
    pub command: AppCommands,
}

/// Subcommands for managing applications and workflows.
#[derive(Debug, Subcommand)]
pub enum AppCommands {
    /// List applications
    List {
        /// Team ID to list apps for (defaults to user apps or default team)
        #[arg(long, short)]
        team_id: Option<String>,

        /// Page number
        #[arg(long, default_value = "1")]
        page: u32,

        /// Number of results per page
        #[arg(long, default_value = "30")]
        page_size: u32,

        /// Filter apps by name
        #[arg(long)]
        name: Option<String>,
    },
    /// Retrieve application details
    Get {
        /// Application ID
        app_id: String,
    },
    /// Register a new Git repository as an application
    Add {
        /// Git repository URL (SSH or HTTPS)
        repository_url: String,

        /// Team ID to register the app under
        #[arg(long, short)]
        team_id: Option<String>,
    },
    /// List workflows available for an application
    Workflows {
        /// Application ID
        app_id: String,
    },
}

/// Dispatches execution of application subcommands.
pub async fn handle_app(
    command: AppCommands,
    client: &CodemagicClient,
    config: &Config,
    output_format: OutputFormat,
) -> Result<()> {
    match command {
        AppCommands::List {
            team_id,
            page,
            page_size,
            name,
        } => {
            let active_team = team_id.or_else(|| config.default_team_id.clone());

            let paginated = if let Some(ref tid) = active_team {
                client
                    .list_team_apps(tid, page, page_size, name.as_deref())
                    .await
                    .with_context(|| format!("Failed to list apps for team '{}'", tid))?
            } else {
                client
                    .list_user_apps(page, page_size, name.as_deref())
                    .await
                    .context("Failed to list user applications")?
            };

            print_output(output_format, &paginated, |p| {
                if let Some(ref tid) = active_team {
                    println!("Applications for Team: {}", tid.cyan());
                } else {
                    println!("Your Personal Applications:");
                }
                render_apps_table(&p.data);
                if p.total_pages > 1 {
                    println!(
                        "Page {} of {} (Page size: {})",
                        p.current_page, p.total_pages, p.page_size
                    );
                }
            });
        }
        AppCommands::Get { app_id } => {
            let workflows = client
                .list_workflows(&app_id)
                .await
                .with_context(|| format!("Failed to retrieve application '{}'", app_id))?;
            let classic_info = client.get_classic_app(&app_id).await.ok().flatten();

            #[derive(serde::Serialize)]
            struct AppDetailOutput {
                app_id: String,
                app_name: Option<String>,
                branches: Vec<String>,
                workflows: Vec<crate::models::Workflow>,
            }

            let detail = AppDetailOutput {
                app_id: app_id.clone(),
                app_name: classic_info.as_ref().and_then(|c| c.app_name.clone()),
                branches: classic_info
                    .as_ref()
                    .and_then(|c| c.branches.clone())
                    .unwrap_or_default(),
                workflows: workflows.clone(),
            };

            print_output(output_format, &detail, |d| {
                println!("\n{}", "=== Application Info ===".bold().cyan());
                println!("{}: {}", "App ID".bold(), d.app_id);
                if let Some(name) = &d.app_name {
                    println!("{}: {}", "App Name".bold(), name);
                }

                if !d.branches.is_empty() {
                    println!("{}: {}", "Branches".bold(), d.branches.join(", "));
                }

                println!("\nWorkflows:");
                render_workflows_table(&d.workflows);
                println!();
            });
        }
        AppCommands::Add {
            repository_url,
            team_id,
        } => {
            let active_team = team_id.or_else(|| config.default_team_id.clone());
            let req = AddAppRequest {
                repository_url: repository_url.clone(),
                team_id: active_team,
            };

            let res = client
                .add_app(&req)
                .await
                .context("Failed to register new application")?;

            print_output(output_format, &res, |_| {
                println!(
                    "{} Application registered successfully for repository: {}",
                    "✓".green(),
                    repository_url.bold()
                );
            });
        }
        AppCommands::Workflows { app_id } => {
            let workflows = client
                .list_workflows(&app_id)
                .await
                .with_context(|| format!("Failed to list workflows for app '{}'", app_id))?;

            print_output(output_format, &workflows, |w| {
                render_workflows_table(w);
            });
        }
    }

    Ok(())
}
