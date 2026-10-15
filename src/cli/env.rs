//! Environment variables and variable groups CLI commands.
//!
//! Provides subcommands to inspect variable groups configured for an application
//! and view the individual environment variables defined within those groups.

use anyhow::{Context, Result};
use clap::{Args, Subcommand};

use crate::client::CodemagicClient;
use crate::output::{
    OutputFormat, print_output, render_variable_groups_table, render_variables_table,
};

/// CLI arguments container for environment variable subcommands.
#[derive(Debug, Args)]
pub struct EnvArgs {
    /// Env subcommand to execute.
    #[command(subcommand)]
    pub command: EnvCommands,
}

/// Subcommands for inspecting environment variables and groups.
#[derive(Debug, Subcommand)]
pub enum EnvCommands {
    /// List variable groups for an application
    Groups {
        /// Application ID
        app_id: String,
    },
    /// List environment variables inside a variable group
    List {
        /// Variable Group ID
        group_id: String,
    },
}

/// Dispatches execution of environment variable subcommands.
pub async fn handle_env(
    command: EnvCommands,
    client: &CodemagicClient,
    output_format: OutputFormat,
) -> Result<()> {
    match command {
        EnvCommands::Groups { app_id } => {
            let groups = client
                .list_variable_groups(&app_id)
                .await
                .with_context(|| format!("Failed to list variable groups for app '{}'", app_id))?;

            print_output(output_format, &groups, |g| {
                render_variable_groups_table(g);
            });
        }
        EnvCommands::List { group_id } => {
            let vars = client
                .list_group_variables(&group_id)
                .await
                .with_context(|| format!("Failed to list variables for group '{}'", group_id))?;

            print_output(output_format, &vars, |v| {
                render_variables_table(v);
            });
        }
    }

    Ok(())
}
