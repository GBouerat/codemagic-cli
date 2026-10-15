//! Build management CLI commands.
//!
//! Provides subcommands to list, inspect, trigger, monitor, and cancel builds
//! on the Codemagic CI/CD platform.

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use std::collections::HashMap;
use std::time::Duration;

use crate::client::{CodemagicClient, ListBuildsFilter};
use crate::config::Config;
use crate::models::{BuildEnvironment, StartBuildRequest};
use crate::output::{
    OutputFormat, format_status, print_output, render_build_detail, render_builds_table,
};

/// CLI arguments container for build subcommands.
#[derive(Debug, Args)]
pub struct BuildArgs {
    /// Build subcommand to execute.
    #[command(subcommand)]
    pub command: BuildCommands,
}

/// Subcommands for managing and inspecting builds.
#[derive(Debug, Subcommand)]
pub enum BuildCommands {
    /// List builds for a team
    List {
        /// Team ID (defaults to configured default team)
        #[arg(long, short)]
        team_id: Option<String>,

        /// Filter builds by application ID (defaults to configured default app if set)
        #[arg(long, short = 'a')]
        app_id: Option<String>,

        /// Filter builds by workflow ID
        #[arg(long)]
        workflow: Option<String>,

        /// Filter builds by branch name
        #[arg(long, short = 'b')]
        branch: Option<String>,

        /// Filter builds by status (e.g. queued, preparing, building, success, failed, canceled)
        #[arg(long, short = 's')]
        status: Option<String>,

        /// Number of results per page
        #[arg(long, default_value = "20")]
        page_size: u32,

        /// Cursor for pagination
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Retrieve details of a specific build
    Get {
        /// Build ID
        build_id: String,
    },
    /// Start a new build
    Start {
        /// Application ID
        app_id: String,

        /// Workflow ID or workflow name as specified in codemagic.yaml or UI
        #[arg(long, short)]
        workflow: String,

        /// Branch to build (mutually exclusive with --tag)
        #[arg(long, short, conflicts_with = "tag")]
        branch: Option<String>,

        /// Git tag to build (mutually exclusive with --branch)
        #[arg(long, short, conflicts_with = "branch")]
        tag: Option<String>,

        /// Build machine instance type (e.g. mac_mini_m2, linux, etc.)
        #[arg(long)]
        machine: Option<String>,

        /// Enable remote SSH access to build machine
        #[arg(long)]
        ssh: bool,

        /// Labels to tag the build with (can be specified multiple times)
        #[arg(long = "label", short = 'l')]
        labels: Vec<String>,

        /// Custom environment variables in KEY=VALUE format (can be specified multiple times)
        #[arg(long = "var", short = 'v')]
        vars: Vec<String>,

        /// Variable group names to include
        #[arg(long = "group", short = 'g')]
        groups: Vec<String>,

        /// Wait and follow build progress until completion
        #[arg(long)]
        wait: bool,
    },
    /// Cancel an in-progress build
    Cancel {
        /// Build ID
        build_id: String,
    },
    /// Live-stream / watch build progress until completion
    Watch {
        /// Build ID
        build_id: String,

        /// Polling interval in seconds (minimum: 1)
        #[arg(long, default_value = "3", value_parser = clap::value_parser!(u64).range(1..))]
        interval: u64,
    },
}

/// Parses a slice of `KEY=VALUE` environment variable strings into a [`HashMap`].
///
/// Trims whitespace from the variable name (`KEY`), rejects empty variable names,
/// and preserves the variable value (`VALUE`) verbatim without stripping whitespace.
pub fn parse_env_vars(vars: &[String]) -> Result<HashMap<String, String>> {
    let mut map = HashMap::with_capacity(vars.len());
    for item in vars {
        let Some((raw_key, value)) = item.split_once('=') else {
            bail!(
                "Invalid environment variable format '{}'. Expected KEY=VALUE.",
                item
            );
        };
        let key = raw_key.trim();
        if key.is_empty() {
            bail!(
                "Environment variable name cannot be empty in '{}'. Expected KEY=VALUE.",
                item
            );
        }
        map.insert(key.to_string(), value.to_string());
    }
    Ok(map)
}

/// Structured output for `codemagic build cancel`.
#[derive(Debug, Clone, serde::Serialize)]
struct CancelBuildOutput {
    /// ID of the build targeted for cancellation.
    build_id: String,
    /// `true` if the build was cancelled; `false` if it had already completed.
    cancelled: bool,
}

/// Dispatches execution of build subcommands.
pub async fn handle_build(
    command: BuildCommands,
    client: &CodemagicClient,
    config: &Config,
    output_format: OutputFormat,
) -> Result<()> {
    match command {
        BuildCommands::List {
            team_id,
            app_id,
            workflow,
            branch,
            status,
            page_size,
            cursor,
        } => {
            let active_team = team_id.or_else(|| config.default_team_id.clone());
            let Some(tid) = active_team else {
                bail!(
                    "Team ID required to list builds. Please pass --team-id <ID> or set default team using 'codemagic auth login'."
                );
            };
            let active_app = app_id.or_else(|| config.default_app_id.clone());

            let filter = ListBuildsFilter {
                page_size,
                cursor: cursor.as_deref(),
                app_id: active_app.as_deref(),
                workflow_id: workflow.as_deref(),
                branch: branch.as_deref(),
                status: status.as_deref(),
            };

            let paginated = client
                .list_team_builds(&tid, &filter)
                .await
                .with_context(|| format!("Failed to list builds for team '{}'", tid))?;

            print_output(output_format, &paginated, |p| {
                println!("Builds for Team: {}", tid.cyan());
                render_builds_table(&p.data);
                if let Some(c) = p.cursor_str() {
                    println!("\nNext cursor: {}", c);
                }
            });
        }
        BuildCommands::Get { build_id } => {
            let build = client
                .get_build(&build_id)
                .await
                .with_context(|| format!("Failed to get build '{}'", build_id))?;

            print_output(output_format, &build, |b| {
                render_build_detail(b);
            });
        }
        BuildCommands::Start {
            app_id,
            workflow,
            branch,
            tag,
            machine,
            ssh,
            labels,
            vars,
            groups,
            wait,
        } => {
            if branch.is_none() && tag.is_none() {
                bail!("Either --branch or --tag must be specified.");
            }

            let env_vars = if !vars.is_empty() {
                Some(parse_env_vars(&vars)?)
            } else {
                None
            };

            let env_groups = if !groups.is_empty() {
                Some(groups)
            } else {
                None
            };

            let environment = if env_vars.is_some() || env_groups.is_some() {
                Some(BuildEnvironment {
                    variables: env_vars,
                    groups: env_groups,
                    software_versions: None,
                })
            } else {
                None
            };

            let req = StartBuildRequest {
                workflow_id: workflow.clone(),
                branch: branch.clone(),
                tag: tag.clone(),
                instance_type: machine,
                ssh_access_enabled: if ssh { Some(true) } else { None },
                labels: if !labels.is_empty() {
                    Some(labels)
                } else {
                    None
                },
                environment,
                inputs: None,
            };

            let started = client
                .start_build(&app_id, &req)
                .await
                .with_context(|| format!("Failed to start build for app '{}'", app_id))?;

            let build_id = started.id.clone();
            if output_format == OutputFormat::Table {
                eprintln!(
                    "{} Build triggered successfully! Build ID: {}",
                    "✓".green(),
                    build_id.bold().cyan()
                );
            }

            if wait {
                if output_format == OutputFormat::Table {
                    eprintln!("{}", "Watching build progress...".cyan());
                }
                watch_build(client, &build_id, 3, output_format).await?;
            } else if output_format == OutputFormat::Table {
                eprintln!(
                    "Use '{}' to monitor status.",
                    format!("codemagic build watch {}", build_id).bold()
                );
            } else {
                print_output(output_format, &started, |_| {});
            }
        }
        BuildCommands::Cancel { build_id } => {
            let cancelled = client
                .cancel_build(&build_id)
                .await
                .with_context(|| format!("Failed to cancel build '{}'", build_id))?;

            let cancel_data = CancelBuildOutput {
                build_id: build_id.clone(),
                cancelled,
            };

            print_output(output_format, &cancel_data, |d| {
                if d.cancelled {
                    println!(
                        "{} Build {} was cancelled successfully.",
                        "✓".green(),
                        d.build_id.bold()
                    );
                } else {
                    println!(
                        "{} Build {} has already finished and could not be cancelled.",
                        "ℹ".yellow(),
                        d.build_id.bold()
                    );
                }
            });
        }
        BuildCommands::Watch { build_id, interval } => {
            watch_build(client, &build_id, interval, output_format).await?;
        }
    }

    Ok(())
}

/// Watches build progression live until completion, tolerating transient network hiccups.
async fn watch_build(
    client: &CodemagicClient,
    build_id: &str,
    interval_secs: u64,
    output_format: OutputFormat,
) -> Result<()> {
    let poll_interval = Duration::from_secs(interval_secs.max(1));

    let spinner = if output_format == OutputFormat::Json {
        ProgressBar::hidden()
    } else {
        let pb = ProgressBar::new_spinner();
        pb.set_style(
            ProgressStyle::default_spinner()
                .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏")
                .template("{spinner:.cyan} {msg}")
                .unwrap(),
        );
        pb.enable_steady_tick(Duration::from_millis(100));
        pb
    };

    let mut last_status = String::new();
    let mut consecutive_network_errors: u32 = 0;
    const MAX_CONSECUTIVE_ERRORS: u32 = 5;

    loop {
        let build = match client.get_build(build_id).await {
            Ok(b) => {
                consecutive_network_errors = 0;
                b
            }
            Err(e) => {
                consecutive_network_errors += 1;
                if consecutive_network_errors >= MAX_CONSECUTIVE_ERRORS {
                    spinner.abandon_with_message(format!(
                        "{} Failed to poll build after {} consecutive network attempts: {}",
                        "✗".red(),
                        MAX_CONSECUTIVE_ERRORS,
                        e
                    ));
                    return Err(e.into());
                }
                tokio::time::sleep(poll_interval).await;
                continue;
            }
        };

        let status = build.status.clone();

        if status != last_status {
            spinner.set_message(format!(
                "Build {}: {}",
                build.id.cyan(),
                format_status(&status)
            ));
            last_status = status.clone();
        }

        match status.to_lowercase().as_str() {
            "success" | "finished" => {
                spinner.finish_with_message(format!(
                    "{} Build {} finished successfully!",
                    "✓".green(),
                    build.id.bold()
                ));
                print_output(output_format, &build, render_build_detail);
                return Ok(());
            }
            "failed" | "errored" | "timeout" => {
                spinner.finish_with_message(format!(
                    "{} Build {} {}",
                    "✗".red(),
                    build.id.bold(),
                    status.to_uppercase().red().bold()
                ));
                print_output(output_format, &build, render_build_detail);
                bail!("Build {} terminated with status '{}'", build_id, status);
            }
            "canceled" => {
                spinner.finish_with_message(format!(
                    "{} Build {} was cancelled.",
                    "■".magenta(),
                    build.id.bold()
                ));
                print_output(output_format, &build, render_build_detail);
                bail!("Build {} was cancelled.", build_id);
            }
            _ => {
                tokio::time::sleep(poll_interval).await;
            }
        }
    }
}
