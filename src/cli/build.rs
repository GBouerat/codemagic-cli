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
    /// Print build step log as plain text
    Log {
        /// Build ID
        build_id: String,

        /// Step selector (1-based index or step name substring; defaults to first failed/timeout/canceled step)
        step: Option<String>,

        /// Print only the last N lines of the log
        #[arg(long)]
        tail: Option<usize>,
    },
    /// Start a new build
    Start {
        /// Application ID (defaults to configured default app if omitted)
        #[arg(value_name = "APP_ID")]
        app_id: Option<String>,

        /// Application ID override (alternative to positional APP_ID)
        #[arg(long = "app", short = 'a')]
        app: Option<String>,

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

/// Structured output representation for `codemagic build log`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BuildLogOutput {
    /// ID of the build.
    pub build_id: String,
    /// Name of the selected step action.
    pub step_name: String,
    /// Execution status of the selected step action.
    pub step_status: String,
    /// Cleaned log lines printed for this step.
    pub lines: Vec<String>,
}

/// Sanitizes raw build step logs by stripping ANSI escape sequences and HTML tags,
/// normalizing carriage returns, and unescaping HTML entities.
pub fn clean_log_text(raw: &str) -> String {
    use std::sync::LazyLock;
    static ANSI_RE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"\x1b\[[0-9;?]*[A-Za-z]").unwrap());
    static TAG_RE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"<[^>]*>").unwrap());

    // 1. Strip ANSI escape sequences
    let without_ansi = ANSI_RE.replace_all(raw, "");
    // 2. Strip HTML tags
    let without_tags = TAG_RE.replace_all(&without_ansi, "");
    // 3. Normalize carriage returns (\r\n and standalone \r to \n)
    let normalized = without_tags.replace("\r\n", "\n").replace('\r', "\n");
    // 4. Unescape HTML entities (e.g. &lt; -> <, &amp; -> &)
    html_escape::decode_html_entities(&normalized).to_string()
}

/// Attempts to detect the current Git branch name via `git rev-parse --abbrev-ref HEAD`.
///
/// Returns `None` if `git` is not installed, not inside a Git repository, or detached HEAD.
pub fn detect_current_git_branch() -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()?;

    if output.status.success() {
        let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if branch.is_empty() || branch == "HEAD" {
            None
        } else {
            Some(branch)
        }
    } else {
        None
    }
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

            let effective_branch = match branch {
                Some(ref b) if b.is_empty() => None, // `--branch ''` lists every branch
                Some(ref b) => Some(b.clone()),
                None => detect_current_git_branch(), // default to current git branch, or None outside repo
            };

            let filter = ListBuildsFilter {
                page_size,
                cursor: cursor.as_deref(),
                app_id: active_app.as_deref(),
                workflow_id: workflow.as_deref(),
                branch: effective_branch.as_deref(),
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
        BuildCommands::Log {
            build_id,
            step,
            tail,
        } => {
            let build = client
                .get_build(&build_id)
                .await
                .with_context(|| format!("Failed to get build '{}'", build_id))?;

            let actions = build.build_actions.as_deref().unwrap_or(&[]);
            if actions.is_empty() {
                bail!("no matching step (see `codemagic build get {}`)", build_id);
            }

            const FAILED_STATUSES: &[&str] = &["failed", "timeout", "canceled", "cancelled"];
            let chosen_action: Option<&crate::models::BuildAction> = match step.as_deref() {
                None => actions.iter().find(|a| {
                    a.status
                        .as_deref()
                        .map(|s| {
                            let s_lower = s.to_lowercase();
                            FAILED_STATUSES.iter().any(|&f| f == s_lower)
                        })
                        .unwrap_or(false)
                }),
                Some(s) => {
                    let s_trimmed = s.trim();
                    if let Ok(num) = s_trimmed.parse::<usize>() {
                        if num >= 1 && num <= actions.len() {
                            actions.get(num - 1)
                        } else {
                            None
                        }
                    } else {
                        let query = s_trimmed.to_lowercase();
                        actions.iter().find(|a| {
                            a.name
                                .as_deref()
                                .map(|n| n.to_lowercase().contains(&query))
                                .unwrap_or(false)
                        })
                    }
                }
            };

            let Some(action) = chosen_action else {
                bail!("no matching step (see `codemagic build get {}`)", build_id);
            };

            let mut urls: Vec<&str> = Vec::new();
            if let Some(ref u) = action.log_url
                && !u.trim().is_empty()
            {
                urls.push(u.as_str());
            }
            if let Some(ref subactions) = action.subactions {
                for sub in subactions {
                    if let Some(ref u) = sub.log_url
                        && !u.trim().is_empty()
                    {
                        urls.push(u.as_str());
                    }
                }
            }

            let action_name = action.name.as_deref().unwrap_or("?");
            let action_status = action.status.as_deref().unwrap_or("?");

            if urls.is_empty() {
                bail!("step '{}' has no log", action_name);
            }

            let mut chunks = Vec::with_capacity(urls.len());
            for url in urls {
                let text = client
                    .fetch_raw_log(url)
                    .await
                    .with_context(|| format!("Failed to fetch log for step '{}'", action_name))?;
                chunks.push(text);
            }

            let combined_raw = chunks.join("\n");
            let cleaned = clean_log_text(&combined_raw);
            let all_lines: Vec<&str> = cleaned.lines().collect();

            let final_lines: Vec<String> = if let Some(n) = tail {
                let start = if n < all_lines.len() {
                    all_lines.len() - n
                } else {
                    0
                };
                all_lines[start..].iter().map(|s| s.to_string()).collect()
            } else {
                all_lines.iter().map(|s| s.to_string()).collect()
            };

            let output_data = BuildLogOutput {
                build_id: build_id.clone(),
                step_name: action_name.to_string(),
                step_status: action_status.to_string(),
                lines: final_lines.clone(),
            };

            print_output(output_format, &output_data, |_| {
                println!("== {} ({})", action_name, action_status);
                for line in &final_lines {
                    println!("{}", line);
                }
            });
        }
        BuildCommands::Start {
            app_id,
            app,
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
            let active_app = app.or(app_id).or_else(|| config.default_app_id.clone());
            let Some(aid) = active_app else {
                bail!(
                    "Application ID required to start a build. Please pass <APP_ID>, --app <ID>, or set 'default_app_id' in config file."
                );
            };

            let effective_branch = if branch.is_none() && tag.is_none() {
                detect_current_git_branch()
            } else {
                branch
            };

            if effective_branch.is_none() && tag.is_none() {
                bail!(
                    "Either --branch or --tag must be specified (or run inside a Git repository to use the current branch)."
                );
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
                branch: effective_branch,
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
                .start_build(&aid, &req)
                .await
                .with_context(|| format!("Failed to start build for app '{}'", aid))?;

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
            "canceled" | "cancelled" => {
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
