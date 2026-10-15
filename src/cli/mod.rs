//! Command-line interface definitions and subcommand enumeration.
//!
//! Powered by [`clap`], this module defines the top-level [`Cli`] parser
//! and dispatches commands to domain-specific submodules.

pub mod app;
pub mod artifact;
pub mod auth;
pub mod build;
pub mod env;

use crate::output::OutputFormat;
use clap::{Parser, Subcommand};

/// Top-level command-line argument parser.
#[derive(Debug, Parser)]
#[command(
    name = "codemagic",
    bin_name = "codemagic",
    version = concat!(env!("CARGO_PKG_VERSION"), "\nhttps://github.com/GBouerat/codemagic-cli"),
    about = "A fast, modern CLI tool for Codemagic CI/CD service",
    long_about = "Interact with Codemagic CI/CD from the command line: manage apps, trigger builds, monitor progress live, download artifacts, and inspect environment variables.",
    after_help = "Repository:\n  https://github.com/GBouerat/codemagic-cli"
)]
pub struct Cli {
    /// Subcommand to execute.
    #[command(subcommand)]
    pub command: Commands,

    /// Codemagic API token override (can also be set via `CODEMAGIC_API_TOKEN` environment variable).
    #[arg(long, global = true, env = "CODEMAGIC_API_TOKEN")]
    pub token: Option<String>,

    /// Output presentation format (`table` or `json`).
    #[arg(long, short = 'o', global = true, default_value = "table")]
    pub output: OutputFormat,
}

/// Enumeration of all top-level subcommands.
#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Manage authentication and API tokens
    Auth(auth::AuthArgs),

    /// List and inspect applications
    App(app::AppArgs),

    /// Trigger, monitor, and cancel builds
    Build(build::BuildArgs),

    /// Inspect and download build artifacts
    Artifact(artifact::ArtifactArgs),

    /// Inspect variable groups and environment variables
    Env(env::EnvArgs),

    /// Generate shell auto-completion script
    Completions {
        /// Target shell
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}
