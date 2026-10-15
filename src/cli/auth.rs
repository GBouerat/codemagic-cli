//! Authentication and API token management CLI commands.
//!
//! Provides subcommands to log in interactively, verify current token status,
//! inspect associated teams, and log out by clearing credentials.

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use colored::Colorize;
use dialoguer::{Input, Password};

use crate::client::CodemagicClient;
use crate::config::Config;
use crate::output::{OutputFormat, print_output, render_teams_table};

/// CLI arguments container for authentication subcommands.
#[derive(Debug, Args)]
pub struct AuthArgs {
    /// Auth subcommand to execute.
    #[command(subcommand)]
    pub command: AuthCommands,
}

/// Subcommands for authentication management.
#[derive(Debug, Subcommand)]
pub enum AuthCommands {
    /// Log in and store your Codemagic API token
    Login {
        /// Codemagic API token (if omitted, will be prompted securely)
        #[arg(long)]
        token: Option<String>,

        /// Optionally specify default team ID
        #[arg(long)]
        team_id: Option<String>,
    },
    /// View authentication status and verify token
    Status,
    /// Log out by clearing stored credentials
    Logout,
}

/// Structured output for `codemagic auth status`.
#[derive(Debug, Clone, serde::Serialize)]
struct StatusOutput {
    /// Whether the user is currently authenticated.
    authenticated: bool,
    /// Authenticated user ID, if logged in.
    user_id: Option<String>,
    /// Default team ID configured, if any.
    default_team_id: Option<String>,
    /// List of teams accessible to the user.
    teams: Vec<crate::models::Team>,
}

/// Structured output for `codemagic auth login`.
#[derive(Debug, Clone, serde::Serialize)]
struct LoginOutput {
    /// Authenticated user ID.
    user_id: String,
    /// Default team ID selected or configured.
    default_team_id: Option<String>,
    /// Path where credentials were saved.
    config_path: String,
}

/// Structured output for `codemagic auth logout`.
#[derive(Debug, Clone, serde::Serialize)]
struct LogoutOutput {
    /// Whether stored credentials were cleared.
    logged_out: bool,
}

/// Dispatches execution of authentication subcommands.
pub async fn handle_auth(
    command: AuthCommands,
    mut config: Config,
    output_format: OutputFormat,
    cli_token: Option<&str>,
) -> Result<()> {
    match command {
        AuthCommands::Login { token, team_id } => {
            let token_str = match token {
                Some(t) if !t.trim().is_empty() => t.trim().to_string(),
                _ => Password::new()
                    .with_prompt("Enter your Codemagic API Token")
                    .interact()
                    .context("Failed to read token input")?,
            };

            let client = CodemagicClient::new(
                token_str.clone(),
                config.v3_base_url.as_deref(),
                config.classic_base_url.as_deref(),
            )?;

            eprintln!("{}", "Verifying token with Codemagic API...".cyan());
            let user = client
                .get_current_user()
                .await
                .context("Token verification failed. Please ensure the token is valid.")?;
            if output_format == OutputFormat::Table {
                println!("{} Logged in as user ID: {}", "✓".green(), user.id.bold());
            }

            let teams = client.get_user_teams().await.unwrap_or_default();
            if !teams.is_empty() && output_format == OutputFormat::Table {
                println!("\nAvailable Teams:");
                render_teams_table(&teams);
            }

            let chosen_team = if let Some(tid) = team_id {
                Some(tid)
            } else if teams.len() == 1 {
                Some(teams[0].id.clone())
            } else if teams.len() > 1 && output_format == OutputFormat::Table {
                let prompt_res: std::result::Result<String, _> = Input::new()
                    .with_prompt("Set default team ID (leave empty to skip)")
                    .allow_empty(true)
                    .interact_text();
                match prompt_res {
                    Ok(val) if !val.trim().is_empty() => Some(val.trim().to_string()),
                    _ => None,
                }
            } else {
                None
            };

            config.api_token = Some(token_str);
            if let Some(tid) = chosen_team {
                config.default_team_id = Some(tid);
            }
            config.save()?;

            let config_path = Config::config_path()?;
            let login_data = LoginOutput {
                user_id: user.id,
                default_team_id: config.default_team_id,
                config_path: config_path.display().to_string(),
            };

            print_output(output_format, &login_data, |d| {
                println!("{} Credentials saved to {}", "✓".green(), d.config_path);
            });
        }
        AuthCommands::Status => {
            let token = Config::resolve_token(cli_token, &config);
            let Some(token) = token else {
                let unauth_data = StatusOutput {
                    authenticated: false,
                    user_id: None,
                    default_team_id: config.default_team_id.clone(),
                    teams: Vec::new(),
                };
                print_output(output_format, &unauth_data, |_| {
                    println!(
                        "{}",
                        "Not logged in. Run 'codemagic auth login' or set CODEMAGIC_API_TOKEN."
                            .yellow()
                    );
                });
                return Ok(());
            };

            let client = CodemagicClient::new(
                token,
                config.v3_base_url.as_deref(),
                config.classic_base_url.as_deref(),
            )?;

            let user = client
                .get_current_user()
                .await
                .context("Failed to verify token with Codemagic API")?;
            let teams = client.get_user_teams().await.unwrap_or_default();

            let status_data = StatusOutput {
                authenticated: true,
                user_id: Some(user.id.clone()),
                default_team_id: config.default_team_id.clone(),
                teams: teams.clone(),
            };

            print_output(output_format, &status_data, |_| {
                println!("{} Authenticated with Codemagic", "✓".green().bold());
                println!("{}: {}", "User ID".bold(), user.id);
                if let Some(team_id) = &config.default_team_id {
                    println!("{}: {}", "Default Team ID".bold(), team_id);
                }
                if !teams.is_empty() {
                    println!("\nTeams:");
                    render_teams_table(&teams);
                }
            });
        }
        AuthCommands::Logout => {
            config.clear_token()?;
            let logout_data = LogoutOutput { logged_out: true };
            print_output(output_format, &logout_data, |_| {
                println!("{} Logged out successfully.", "✓".green());
            });
        }
    }

    Ok(())
}
