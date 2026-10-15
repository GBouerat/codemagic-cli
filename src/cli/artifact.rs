//! Artifact inspection, download, and public sharing CLI commands.

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::client::CodemagicClient;
use crate::output::{OutputFormat, format_size, print_output, render_artifacts_table};

/// CLI arguments container for artifact subcommands.
#[derive(Debug, Args)]
pub struct ArtifactArgs {
    /// Artifact subcommand to execute.
    #[command(subcommand)]
    pub command: ArtifactCommands,
}

/// Subcommands for inspecting and downloading artifacts.
#[derive(Debug, Subcommand)]
pub enum ArtifactCommands {
    /// List artifacts generated for a build
    List {
        /// Build ID
        build_id: String,
    },
    /// Download artifact(s) for a build
    Download {
        /// Build ID
        build_id: String,

        /// Specific artifact name to download (if omitted, all artifacts will be downloaded)
        #[arg(long, short)]
        name: Option<String>,

        /// Destination directory or output file path (defaults to current directory)
        #[arg(long = "dest", short = 'd', alias = "out", alias = "destination")]
        dest: Option<PathBuf>,
    },
    /// Generate an expiring public download link for an artifact
    Share {
        /// Build ID
        build_id: String,

        /// Artifact name to share
        #[arg(long, short)]
        name: String,

        /// Expiration time in seconds from now (default: 86400 = 24 hours)
        #[arg(long, default_value = "86400")]
        expires_in: u64,
    },
}

/// Validates and extracts a safe, flat filename from an artifact name.
///
/// Rejects path traversal sequences (`..`), root/prefix paths (`/etc/passwd`),
/// and empty names, while extracting the terminal filename from benign relative paths
/// (e.g. `"build/outputs/apk/app-release.apk"` -> `"app-release.apk"`).
pub fn sanitize_artifact_filename(raw_name: &str) -> Result<String> {
    let trimmed = raw_name.trim();
    if trimmed.is_empty() {
        bail!("Artifact filename cannot be empty");
    }

    let path = Path::new(trimmed);
    for component in path.components() {
        match component {
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                bail!(
                    "Unsafe artifact filename '{}' rejected (path traversal or absolute path detected)",
                    raw_name
                );
            }
            Component::Normal(_) | Component::CurDir => {}
        }
    }

    let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
        bail!("Invalid artifact filename '{}'", raw_name);
    };

    if file_name.is_empty() || file_name == "." || file_name == ".." {
        bail!("Invalid artifact filename '{}'", raw_name);
    }

    Ok(file_name.to_string())
}

/// Summary of a downloaded artifact used for structured JSON output.
#[derive(Debug, Clone, serde::Serialize)]
struct DownloadedArtifactSummary {
    /// Original artifact name.
    name: String,
    /// Local filesystem path where the artifact was saved.
    path: String,
    /// Size of the artifact in bytes.
    size_in_bytes: u64,
}

/// Dispatches execution of artifact subcommands.
pub async fn handle_artifact(
    command: ArtifactCommands,
    client: &CodemagicClient,
    output_format: OutputFormat,
) -> Result<()> {
    match command {
        ArtifactCommands::List { build_id } => {
            let build = client
                .get_build(&build_id)
                .await
                .with_context(|| format!("Failed to retrieve build '{}'", build_id))?;

            let artifacts = build.artifacts.unwrap_or_default();
            print_output(output_format, &artifacts, |items| {
                println!("Artifacts for Build {}:", build_id.cyan());
                render_artifacts_table(items);
            });
        }
        ArtifactCommands::Download {
            build_id,
            name,
            dest,
        } => {
            let build = client
                .get_build(&build_id)
                .await
                .with_context(|| format!("Failed to retrieve build '{}'", build_id))?;

            let artifacts = build.artifacts.unwrap_or_default();
            if artifacts.is_empty() {
                let empty: Vec<DownloadedArtifactSummary> = Vec::new();
                print_output(output_format, &empty, |_| {
                    println!("{}", "No artifacts found for this build.".yellow());
                });
                return Ok(());
            }

            let target_artifacts: Vec<_> = if let Some(ref target_name) = name {
                let filtered: Vec<_> = artifacts
                    .into_iter()
                    .filter(|a| a.name == *target_name)
                    .collect();
                if filtered.is_empty() {
                    bail!(
                        "No artifact named '{}' found in build {}.",
                        target_name,
                        build_id
                    );
                }
                filtered
            } else {
                artifacts
            };

            let is_single_file_target =
                target_artifacts.len() == 1 && name.is_some() && dest.is_some();

            let mut downloaded_summaries = Vec::new();

            for artifact in target_artifacts {
                let safe_filename = sanitize_artifact_filename(&artifact.name)?;

                let file_path = match &dest {
                    Some(out) if is_single_file_target && !out.is_dir() => {
                        if let Some(parent) = out.parent()
                            && !parent.as_os_str().is_empty()
                        {
                            std::fs::create_dir_all(parent).with_context(|| {
                                format!("Failed to create parent directory {:?}", parent)
                            })?;
                        }
                        out.clone()
                    }
                    Some(dir) => {
                        if !dir.exists() {
                            std::fs::create_dir_all(dir).with_context(|| {
                                format!("Failed to create output directory {:?}", dir)
                            })?;
                        }
                        dir.join(&safe_filename)
                    }
                    None => PathBuf::from(".").join(&safe_filename),
                };

                let url = match artifact.short_lived_download_url.as_deref() {
                    Some(u) => u,
                    None => {
                        eprintln!(
                            "{} Artifact '{}' has no download URL (it may be expired).",
                            "⚠".yellow(),
                            artifact.name
                        );
                        continue;
                    }
                };

                let pb = if output_format == OutputFormat::Json {
                    ProgressBar::hidden()
                } else {
                    let bar = ProgressBar::new(artifact.size());
                    bar.set_style(
                        ProgressStyle::default_bar()
                            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta}) {msg}")
                            .unwrap()
                            .progress_chars("#>-"),
                    );
                    bar.set_message(format!("Downloading {}", artifact.name));
                    bar
                };

                let pb_clone = pb.clone();
                let dl_res = client
                    .download_file(
                        url,
                        &file_path,
                        Some(move |cur, _total| {
                            pb_clone.set_position(cur);
                        }),
                    )
                    .await;

                if let Err(e) = dl_res {
                    pb.abandon_with_message(format!("Failed downloading {}: {}", artifact.name, e));
                    return Err(e.into());
                } else {
                    pb.finish_with_message(format!(
                        "Saved {} ({}) to {}",
                        artifact.name.green().bold(),
                        format_size(artifact.size()),
                        file_path.display()
                    ));
                    downloaded_summaries.push(DownloadedArtifactSummary {
                        name: artifact.name.clone(),
                        path: file_path.display().to_string(),
                        size_in_bytes: artifact.size(),
                    });
                }
            }

            if output_format == OutputFormat::Json {
                print_output(output_format, &downloaded_summaries, |_| {});
            }
        }
        ArtifactCommands::Share {
            build_id,
            name,
            expires_in,
        } => {
            let build = client
                .get_build(&build_id)
                .await
                .with_context(|| format!("Failed to retrieve build '{}'", build_id))?;

            let artifacts = build.artifacts.unwrap_or_default();
            let Some(artifact) = artifacts.into_iter().find(|a| a.name == name) else {
                bail!("Artifact '{}' not found in build {}.", name, build_id);
            };

            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let expires_at = now + expires_in;

            let dl_url = artifact
                .short_lived_download_url
                .as_deref()
                .unwrap_or(&artifact.name);
            let path_part = if let Some(idx) = dl_url.find("/artifacts/") {
                &dl_url[idx + "/artifacts/".len()..]
            } else {
                dl_url
            };

            let public_resp = client
                .create_public_artifact_url(path_part, expires_at)
                .await
                .context("Failed to generate public download URL")?;

            print_output(output_format, &public_resp, |resp| {
                println!("{} Public download link created successfully!", "✓".green());
                println!("{}: {}", "Artifact".bold(), name);
                println!("{}: {}", "Public URL".bold().cyan(), resp.url);
                if let Some(exp) = &resp.expires_at {
                    println!("{}: {}", "Expires At".bold(), exp);
                }
            });
        }
    }

    Ok(())
}
