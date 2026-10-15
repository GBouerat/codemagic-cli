//! Binary entry point for the `codemagic` CLI application.

use anyhow::Result;
use clap::Parser;
use colored::Colorize;

use codemagic::cli::{self, Cli, Commands};
use codemagic::client::CodemagicClient;
use codemagic::config::Config;

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("{} {:#}", "Error:".red().bold(), err);
        std::process::exit(1);
    }
}

/// Parses command-line arguments, resolves configuration, and dispatches to subcommand handlers.
async fn run() -> Result<()> {
    let cli = Cli::parse();
    let config = Config::load().unwrap_or_default();
    let output = cli.output;
    let cli_token = cli.token;

    match cli.command {
        Commands::Auth(auth_args) => {
            cli::auth::handle_auth(auth_args.command, config, output, cli_token.as_deref()).await?;
        }
        Commands::App(app_args) => {
            let client = build_client(cli_token.as_deref(), &config)?;
            cli::app::handle_app(app_args.command, &client, &config, output).await?;
        }
        Commands::Build(build_args) => {
            let client = build_client(cli_token.as_deref(), &config)?;
            cli::build::handle_build(build_args.command, &client, &config, output).await?;
        }
        Commands::Artifact(artifact_args) => {
            let client = build_client(cli_token.as_deref(), &config)?;
            cli::artifact::handle_artifact(artifact_args.command, &client, output).await?;
        }
        Commands::Env(env_args) => {
            let client = build_client(cli_token.as_deref(), &config)?;
            cli::env::handle_env(env_args.command, &client, output).await?;
        }
        Commands::Completions { shell } => {
            use clap::CommandFactory;
            let mut cmd = Cli::command();
            clap_complete::generate(shell, &mut cmd, "codemagic", &mut std::io::stdout());
        }
    }

    Ok(())
}

/// Constructs an authenticated [`CodemagicClient`] using the resolved API token.
///
/// Halts with an actionable error if no token can be found across CLI arguments,
/// environment variables, or local configuration files.
fn build_client(cli_token: Option<&str>, config: &Config) -> Result<CodemagicClient> {
    let token = Config::resolve_token(cli_token, config);
    let Some(token) = token else {
        anyhow::bail!(
            "No Codemagic API token found.\n\
             Please run '{}' or provide the token via the '--token' flag or 'CODEMAGIC_API_TOKEN' environment variable.",
            "codemagic auth login".bold().cyan()
        );
    };

    let client = CodemagicClient::new(
        token,
        config.v3_base_url.as_deref(),
        config.classic_base_url.as_deref(),
    )?;

    Ok(client)
}
