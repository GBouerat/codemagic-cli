//! Output formatting, tables, and JSON presentation layer.
//!
//! Provides the [`OutputFormat`] enum and helper rendering functions for
//! formatting applications, builds, artifacts, workflows, and variable groups
//! using either human-friendly styled terminal tables or raw JSON.

use clap::ValueEnum;
use colored::{ColoredString, Colorize};
use comfy_table::modifiers::UTF8_ROUND_CORNERS;
use comfy_table::presets::UTF8_FULL;
use comfy_table::{Cell, Color, ContentArrangement, Table};
use serde::Serialize;

use crate::models::{App, Artifact, Build, EnvironmentVariable, Team, VariableGroup, Workflow};

/// Format modes supported by the `--output` CLI argument.
#[derive(Debug, Clone, Copy, ValueEnum, Default, PartialEq, Eq)]
pub enum OutputFormat {
    /// Formatted terminal tables with color coding.
    #[default]
    Table,
    /// Raw, pretty-printed JSON suitable for piping to `jq`.
    Json,
}

/// Formats a build status string with colored terminal text.
///
/// - `success`, `finished` -> Green bold
/// - `failed`, `errored`, `timeout` -> Red bold
/// - `building`, `running`, `in_progress` -> Yellow bold
/// - `canceled` -> Magenta bold
/// - `queued`, `preparing` -> Blue bold
pub fn format_status(status: &str) -> ColoredString {
    match status.to_lowercase().as_str() {
        "success" | "finished" => status.green().bold(),
        "failed" | "errored" | "timeout" => status.red().bold(),
        "building" | "running" | "in_progress" => status.yellow().bold(),
        "canceled" => status.magenta().bold(),
        "queued" | "preparing" => status.blue().bold(),
        _ => status.white(),
    }
}

/// Maps a build status string to a [`comfy_table::Color`] for cell-level table styling.
pub fn status_color(status: &str) -> Color {
    match status.to_lowercase().as_str() {
        "success" | "finished" => Color::Green,
        "failed" | "errored" | "timeout" => Color::Red,
        "building" | "running" | "in_progress" => Color::Yellow,
        "canceled" => Color::Magenta,
        "queued" | "preparing" => Color::Blue,
        _ => Color::White,
    }
}

/// Converts a byte count into a human-readable size string (B, KB, MB, GB).
pub fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

/// Dispatches presentation to either JSON stdout or a custom table rendering callback.
pub fn print_output<T: Serialize>(format: OutputFormat, data: &T, table_fn: impl FnOnce(&T)) {
    match format {
        OutputFormat::Json => {
            if let Ok(json) = serde_json::to_string_pretty(data) {
                println!("{}", json);
            }
        }
        OutputFormat::Table => {
            table_fn(data);
        }
    }
}

/// Renders a formatted terminal table listing applications.
pub fn render_apps_table(apps: &[App]) {
    if apps.is_empty() {
        println!("{}", "No applications found.".yellow());
        return;
    }

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .apply_modifier(UTF8_ROUND_CORNERS)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec!["App ID", "Name", "Archived", "Last Build ID"]);

    for app in apps {
        let (archived_text, archived_color) = match app.archived {
            Some(true) => ("Yes", Color::Red),
            _ => ("No", Color::Green),
        };
        table.add_row(vec![
            Cell::new(&app.id).fg(Color::Cyan),
            Cell::new(&app.name).fg(Color::White),
            Cell::new(archived_text).fg(archived_color),
            Cell::new(app.last_build_id.as_deref().unwrap_or("-")).fg(Color::DarkGrey),
        ]);
    }

    println!("{table}");
}

/// Renders a formatted terminal table listing teams.
pub fn render_teams_table(teams: &[Team]) {
    if teams.is_empty() {
        println!("{}", "No teams found.".yellow());
        return;
    }

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .apply_modifier(UTF8_ROUND_CORNERS)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec!["Team ID", "Team Name"]);

    for team in teams {
        table.add_row(vec![
            Cell::new(&team.id).fg(Color::Cyan),
            Cell::new(&team.name).fg(Color::White),
        ]);
    }

    println!("{table}");
}

/// Renders a formatted terminal table listing builds.
pub fn render_builds_table(builds: &[Build]) {
    if builds.is_empty() {
        println!("{}", "No builds found.".yellow());
        return;
    }

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .apply_modifier(UTF8_ROUND_CORNERS)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec![
            "Build ID",
            "Status",
            "Workflow",
            "Branch / Tag",
            "Commit",
            "Started At",
            "Finished At",
        ]);

    for b in builds {
        let branch_or_tag = b.branch.as_deref().or(b.tag.as_deref()).unwrap_or("-");

        let workflow_name = b
            .workflow
            .as_ref()
            .and_then(|w| w.name.as_deref())
            .or_else(|| b.workflow.as_ref().map(|w| w.id.as_str()))
            .unwrap_or("-");

        let commit_hash = b
            .commit
            .as_ref()
            .and_then(|c| c.hash.as_deref())
            .map(|h| h.char_indices().nth(7).map_or(h, |(idx, _)| &h[..idx]))
            .unwrap_or("-");

        let status_c = status_color(&b.status);

        table.add_row(vec![
            Cell::new(&b.id).fg(Color::Cyan),
            Cell::new(&b.status).fg(status_c),
            Cell::new(workflow_name),
            Cell::new(branch_or_tag),
            Cell::new(commit_hash).fg(Color::DarkGrey),
            Cell::new(b.started_at.as_deref().unwrap_or("-")),
            Cell::new(b.finished_at.as_deref().unwrap_or("-")),
        ]);
    }

    println!("{table}");
}

/// Renders detailed metadata for a single build, including commit info and artifacts.
pub fn render_build_detail(build: &Build) {
    println!("\n{}", "=== Build Details ===".bold().cyan());
    println!("{}: {}", "Build ID".bold(), build.id);
    println!("{}: {}", "Status".bold(), format_status(&build.status));
    if let Some(app_id) = &build.app_id {
        println!("{}: {}", "App ID".bold(), app_id);
    }
    if let Some(index) = build.index {
        println!("{}: #{}", "Build Number".bold(), index);
    }
    if let Some(ref wf) = build.workflow {
        let name = wf.name.as_deref().unwrap_or(&wf.id);
        println!("{}: {} ({})", "Workflow".bold(), name, wf.id);
    }
    if let Some(branch) = &build.branch {
        println!("{}: {}", "Branch".bold(), branch);
    }
    if let Some(tag) = &build.tag {
        println!("{}: {}", "Tag".bold(), tag);
    }
    if let Some(instance) = &build.instance_type {
        println!("{}: {}", "Machine Type".bold(), instance);
    }
    if let Some(ref commit) = build.commit {
        if let Some(hash) = &commit.hash {
            println!("{}: {}", "Commit Hash".bold(), hash);
        }
        if let Some(msg) = &commit.message {
            println!(
                "{}: {}",
                "Commit Message".bold(),
                msg.lines().next().unwrap_or("")
            );
        }
        if let Some(author) = &commit.author_name {
            println!("{}: {}", "Author".bold(), author);
        }
    }
    if let Some(started) = &build.started_at {
        println!("{}: {}", "Started At".bold(), started);
    }
    if let Some(finished) = &build.finished_at {
        println!("{}: {}", "Finished At".bold(), finished);
    }
    if let Some(notes) = build.release_notes_summary() {
        println!("{}: {}", "Release Notes".bold(), notes);
    }

    if let Some(ref artifacts) = build.artifacts
        && !artifacts.is_empty()
    {
        println!("\n{}", "--- Artifacts ---".bold());
        render_artifacts_table(artifacts);
    }
    println!();
}

/// Renders a formatted terminal table listing artifacts.
pub fn render_artifacts_table(artifacts: &[Artifact]) {
    if artifacts.is_empty() {
        println!("{}", "No artifacts generated.".yellow());
        return;
    }

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .apply_modifier(UTF8_ROUND_CORNERS)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec!["Artifact Name", "Size", "Type", "Version"]);

    for a in artifacts {
        let ver = match (&a.version_name, &a.version_code) {
            (Some(name), Some(code)) => format!("{name} ({code})"),
            (Some(name), None) => name.clone(),
            (None, Some(code)) => code.clone(),
            (None, None) => "-".to_string(),
        };

        table.add_row(vec![
            Cell::new(&a.name).fg(Color::Cyan),
            Cell::new(format_size(a.size())),
            Cell::new(a.artifact_type.as_deref().unwrap_or("-")),
            Cell::new(ver),
        ]);
    }

    println!("{table}");
}

/// Renders a formatted terminal table listing workflows.
pub fn render_workflows_table(workflows: &[Workflow]) {
    if workflows.is_empty() {
        println!("{}", "No workflows found.".yellow());
        return;
    }

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .apply_modifier(UTF8_ROUND_CORNERS)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec!["Workflow ID", "Name", "Source"]);

    for w in workflows {
        table.add_row(vec![
            Cell::new(&w.id).fg(Color::Cyan),
            Cell::new(w.name.as_deref().unwrap_or("-")).fg(Color::White),
            Cell::new(&w.source).fg(Color::DarkGrey),
        ]);
    }

    println!("{table}");
}

/// Renders a formatted terminal table listing variable groups.
pub fn render_variable_groups_table(groups: &[VariableGroup]) {
    if groups.is_empty() {
        println!("{}", "No variable groups found.".yellow());
        return;
    }

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .apply_modifier(UTF8_ROUND_CORNERS)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec!["Group ID", "Name"]);

    for g in groups {
        table.add_row(vec![
            Cell::new(&g.id).fg(Color::Cyan),
            Cell::new(&g.name).fg(Color::White),
        ]);
    }

    println!("{table}");
}

/// Renders a formatted terminal table listing environment variables.
pub fn render_variables_table(vars: &[EnvironmentVariable]) {
    if vars.is_empty() {
        println!("{}", "No variables found in this group.".yellow());
        return;
    }

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .apply_modifier(UTF8_ROUND_CORNERS)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec!["ID", "Name", "Value", "Secure"]);

    for v in vars {
        let val_cell = if v.secure {
            Cell::new("******").fg(Color::DarkGrey)
        } else {
            Cell::new(v.value.as_deref().unwrap_or("-"))
        };
        let (secure_text, secure_color) = if v.secure {
            ("Yes", Color::Green)
        } else {
            ("No", Color::Yellow)
        };

        table.add_row(vec![
            Cell::new(&v.id).fg(Color::Cyan),
            Cell::new(&v.name).fg(Color::White),
            val_cell,
            Cell::new(secure_text).fg(secure_color),
        ]);
    }

    println!("{table}");
}
