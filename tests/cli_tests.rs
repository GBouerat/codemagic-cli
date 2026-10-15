//! Exhaustive integration and unit test suite for `codemagic-cli`.

use clap::Parser;
use comfy_table::Color;
use std::collections::HashMap;
use std::sync::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use codemagic::cli::artifact::sanitize_artifact_filename;
use codemagic::cli::build::parse_env_vars;
use codemagic::cli::{Cli, Commands};
use codemagic::client::error::CodemagicError;
use codemagic::client::{CodemagicClient, ListBuildsFilter, url_encode};
use codemagic::config::Config;
use codemagic::models::{
    AddAppRequest, App, Artifact, Build, BuildEnvironment, ClassicAppInfo, ClassicAppResponse,
    ClassicPagination, CommitInfo, CreatePublicUrlRequest, CreatePublicUrlResponse,
    CursorPagination, DataWrapper, EnvironmentVariable, StartBuildRequest, Team, User,
    VariableGroup, Workflow, WorkflowRef,
};
use codemagic::output::{
    OutputFormat, format_size, format_status, render_apps_table, render_artifacts_table,
    render_build_detail, render_builds_table, render_teams_table, render_variable_groups_table,
    render_variables_table, render_workflows_table, status_color,
};

/// Global mutex to synchronize tests that read or mutate `CODEMAGIC_API_TOKEN`
/// in the process environment, preventing multi-threaded data races.
static ENV_LOCK: Mutex<()> = Mutex::new(());

// =============================================================================
// Configuration & Token Precedence Tests
// =============================================================================

#[test]
fn test_token_resolution_order() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let config = Config {
        api_token: Some("config_token".to_string()),
        ..Default::default()
    };

    // 1. Explicit CLI token has top priority
    let token = Config::resolve_token(Some("cli_token"), &config);
    assert_eq!(token.as_deref(), Some("cli_token"));

    // 2. Fallback to config when no env or cli
    unsafe {
        std::env::remove_var("CODEMAGIC_API_TOKEN");
    }
    let token = Config::resolve_token(None, &config);
    assert_eq!(token.as_deref(), Some("config_token"));

    // 3. Environment variable priority over config
    unsafe {
        std::env::set_var("CODEMAGIC_API_TOKEN", "env_token");
    }
    let token = Config::resolve_token(None, &config);
    assert_eq!(token.as_deref(), Some("env_token"));

    // Cleanup
    unsafe {
        std::env::remove_var("CODEMAGIC_API_TOKEN");
    }
}

#[test]
fn test_token_resolution_whitespace_and_empty() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        std::env::remove_var("CODEMAGIC_API_TOKEN");
    }

    let config = Config {
        api_token: Some("  config_token_trimmed  ".to_string()),
        ..Default::default()
    };

    // Empty or whitespace CLI token should be ignored and fall back
    let token = Config::resolve_token(Some("   "), &config);
    assert_eq!(token.as_deref(), Some("config_token_trimmed"));

    let empty_config = Config::default();
    assert_eq!(Config::resolve_token(None, &empty_config), None);
    assert_eq!(Config::resolve_token(Some(""), &empty_config), None);

    // Config with whitespace-only token should also resolve to None
    let whitespace_config = Config {
        api_token: Some("   ".to_string()),
        ..Default::default()
    };
    assert_eq!(Config::resolve_token(None, &whitespace_config), None);
}

#[test]
fn test_config_serialization_roundtrip_and_permissions() {
    let config = Config {
        api_token: Some("secret_token_123".to_string()),
        default_team_id: Some("team_abc".to_string()),
        default_app_id: Some("app_xyz".to_string()),
        v3_base_url: Some("https://custom.codemagic.io".to_string()),
        classic_base_url: Some("https://custom-api.codemagic.io".to_string()),
    };

    let tmp_dir = std::env::temp_dir().join(format!("codemagic_test_{}", std::process::id()));
    let cfg_path = tmp_dir.join("nested").join("config.toml");

    // Non-existent path returns default config
    let _ = std::fs::remove_dir_all(&tmp_dir);
    assert_eq!(Config::load_from(&cfg_path).unwrap(), Config::default());

    // Save and reload
    config
        .save_to(&cfg_path)
        .expect("failed to save config to temp path");
    let loaded = Config::load_from(&cfg_path).expect("failed to load config from temp path");
    assert_eq!(loaded, config);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&cfg_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "config.toml must have 0600 permissions on Unix"
        );
    }

    let _ = std::fs::remove_dir_all(&tmp_dir);
}

// =============================================================================
// Input Sanitization, Env Var Parsing & URL Encoding Tests
// =============================================================================

#[test]
fn test_sanitize_artifact_filename_exhaustive() {
    // Valid simple filenames
    assert_eq!(
        sanitize_artifact_filename("app-release.apk").unwrap(),
        "app-release.apk"
    );
    assert_eq!(
        sanitize_artifact_filename("  Runner.ipa  ").unwrap(),
        "Runner.ipa"
    );

    // Valid nested relative path extracts flat filename
    assert_eq!(
        sanitize_artifact_filename("build/outputs/apk/release/app-release.apk").unwrap(),
        "app-release.apk"
    );
    assert_eq!(
        sanitize_artifact_filename("./artifacts/app.aab").unwrap(),
        "app.aab"
    );

    // Path traversal & absolute paths must be rejected
    assert!(sanitize_artifact_filename("../../etc/passwd").is_err());
    assert!(sanitize_artifact_filename("../evil.sh").is_err());
    assert!(sanitize_artifact_filename("sub/../../evil.sh").is_err());
    assert!(sanitize_artifact_filename("/etc/passwd").is_err());
    assert!(sanitize_artifact_filename("/tmp/app.apk").is_err());
    assert!(sanitize_artifact_filename("").is_err());
    assert!(sanitize_artifact_filename("   ").is_err());
    assert!(sanitize_artifact_filename(".").is_err());
    assert!(sanitize_artifact_filename("..").is_err());
}

#[test]
fn test_parse_env_vars_exhaustive() {
    let inputs = vec![
        "API_URL=https://api.example.com".to_string(),
        "  TRIMMED_KEY  =  value with spaces  ".to_string(),
        "BASE64_TOKEN=abc=def==".to_string(),
        "EMPTY_VAL=".to_string(),
    ];

    let parsed = parse_env_vars(&inputs).expect("valid env vars");
    assert_eq!(
        parsed.get("API_URL").map(String::as_str),
        Some("https://api.example.com")
    );
    assert_eq!(
        parsed.get("TRIMMED_KEY").map(String::as_str),
        Some("  value with spaces  ")
    );
    assert_eq!(
        parsed.get("BASE64_TOKEN").map(String::as_str),
        Some("abc=def==")
    );
    assert_eq!(parsed.get("EMPTY_VAL").map(String::as_str), Some(""));

    // Invalid formats
    assert!(parse_env_vars(&["MISSING_EQUALS".to_string()]).is_err());
    assert!(parse_env_vars(&["=empty_key".to_string()]).is_err());
    assert!(parse_env_vars(&["   =whitespace_key".to_string()]).is_err());
}

#[test]
fn test_url_encode_exhaustive() {
    assert_eq!(url_encode("simple-name_1.0~test"), "simple-name_1.0~test");
    assert_eq!(url_encode("Flutter App"), "Flutter%20App");
    assert_eq!(
        url_encode("feature/auth&login=1"),
        "feature%2Fauth%26login%3D1"
    );
}

// =============================================================================
// Output Formatting & Utilities Tests
// =============================================================================

#[test]
fn test_format_size_exhaustive() {
    assert_eq!(format_size(0), "0 B");
    assert_eq!(format_size(500), "500 B");
    assert_eq!(format_size(1023), "1023 B");
    assert_eq!(format_size(1024), "1.00 KB");
    assert_eq!(format_size(1536), "1.50 KB");
    assert_eq!(format_size(1024 * 1024), "1.00 MB");
    assert_eq!(format_size(1024 * 1024 * 5 + 512 * 1024), "5.50 MB");
    assert_eq!(format_size(1024 * 1024 * 1024), "1.00 GB");
    assert_eq!(format_size(1024 * 1024 * 1024 * 3), "3.00 GB");
}

#[test]
fn test_format_status_and_colors() {
    let statuses = [
        ("success", Color::Green),
        ("finished", Color::Green),
        ("SUCCESS", Color::Green),
        ("failed", Color::Red),
        ("errored", Color::Red),
        ("timeout", Color::Red),
        ("building", Color::Yellow),
        ("running", Color::Yellow),
        ("in_progress", Color::Yellow),
        ("canceled", Color::Magenta),
        ("queued", Color::Blue),
        ("preparing", Color::Blue),
        ("unknown_status", Color::White),
    ];

    for (status, expected_color) in statuses {
        assert_eq!(status_color(status), expected_color);
        let formatted = format_status(status);
        assert!(!formatted.to_string().is_empty());
    }
}

// =============================================================================
// Polymorphic Deserialization & Helper Methods Tests
// =============================================================================

#[test]
fn test_polymorphic_release_notes_exhaustive() {
    // 1. String release notes
    let build_str = Build {
        id: "b1".to_string(),
        app_id: None,
        index: None,
        status: "finished".to_string(),
        branch: None,
        tag: None,
        workflow: None,
        commit: None,
        instance_type: None,
        labels: None,
        artifacts: None,
        created_at: None,
        started_at: None,
        finished_at: None,
        release_notes: Some(serde_json::Value::String("Version 1.0 notes".to_string())),
    };
    assert_eq!(
        build_str.release_notes_summary().as_deref(),
        Some("Version 1.0 notes")
    );

    // 2. Empty string release notes
    let build_empty_str = Build {
        release_notes: Some(serde_json::Value::String("   ".to_string())),
        ..build_str.clone()
    };
    assert_eq!(build_empty_str.release_notes_summary(), None);

    // 3. Array of objects with "text"
    let build_arr_text = Build {
        release_notes: Some(serde_json::json!([
            {"language": "en", "text": "Feature A added"},
            {"language": "es", "text": "Caracteristica A agregada"}
        ])),
        ..build_str.clone()
    };
    assert_eq!(
        build_arr_text.release_notes_summary().as_deref(),
        Some("Feature A added\nCaracteristica A agregada")
    );

    // 4. Array of objects with "note"
    let build_arr_note = Build {
        release_notes: Some(serde_json::json!([
            {"note": "Bug fix 1"}
        ])),
        ..build_str.clone()
    };
    assert_eq!(
        build_arr_note.release_notes_summary().as_deref(),
        Some("Bug fix 1")
    );

    // 5. Empty array
    let build_arr_empty = Build {
        release_notes: Some(serde_json::json!([])),
        ..build_str.clone()
    };
    assert_eq!(build_arr_empty.release_notes_summary(), None);

    // 6. None / null
    let build_none = Build {
        release_notes: None,
        ..build_str.clone()
    };
    assert_eq!(build_none.release_notes_summary(), None);
}

#[test]
fn test_cursor_pagination_formats() {
    // 1. Plain string cursor
    let pag_str: CursorPagination<String> = serde_json::from_str(
        r#"{
        "data": ["item1"],
        "page_size": 20,
        "cursor": "cursor_abc_123"
    }"#,
    )
    .expect("valid pagination json");
    assert_eq!(pag_str.cursor_str().as_deref(), Some("cursor_abc_123"));

    // 2. MongoDB $oid object cursor
    let pag_oid: CursorPagination<String> = serde_json::from_str(
        r#"{
        "data": ["item1"],
        "page_size": 20,
        "cursor": { "$oid": "6ad0f15212cce3cb996d8f8d" }
    }"#,
    )
    .expect("valid pagination json");
    assert_eq!(
        pag_oid.cursor_str().as_deref(),
        Some("6ad0f15212cce3cb996d8f8d")
    );

    // 3. Null cursor
    let pag_null: CursorPagination<String> = serde_json::from_str(
        r#"{
        "data": [],
        "page_size": 20,
        "cursor": null
    }"#,
    )
    .expect("valid pagination json");
    assert_eq!(pag_null.cursor_str(), None);
}

#[test]
fn test_artifact_size_helper() {
    let art_with_size = Artifact {
        name: "test.apk".to_string(),
        size_in_bytes: Some(1048576),
        artifact_type: Some("apk".to_string()),
        short_lived_download_url: None,
        version_code: None,
        version_name: None,
    };
    assert_eq!(art_with_size.size(), 1048576);

    let art_no_size = Artifact {
        name: "test.apk".to_string(),
        size_in_bytes: None,
        artifact_type: None,
        short_lived_download_url: None,
        version_code: None,
        version_name: None,
    };
    assert_eq!(art_no_size.size(), 0);
}

// =============================================================================
// API Payload Deserialization Tests
// =============================================================================

#[test]
fn test_deserialization_v3_pagination() {
    let json_data = r#"{
        "data": [
            {
                "id": "60a0b1c2d3e4f56789abcdef",
                "name": "iOS Production App",
                "icon_url": "https://example.com/icon.png",
                "last_build_id": "61b1c2d3e4f56789abcdef01",
                "archived": false
            }
        ],
        "current_page": 1,
        "page_size": 30,
        "total_pages": 1
    }"#;

    let paginated: ClassicPagination<App> = serde_json::from_str(json_data).unwrap();
    assert_eq!(paginated.data.len(), 1);
    assert_eq!(paginated.data[0].id, "60a0b1c2d3e4f56789abcdef");
    assert_eq!(paginated.data[0].name, "iOS Production App");
    assert_eq!(paginated.data[0].archived, Some(false));
}

#[test]
fn test_deserialization_v3_workflows() {
    let json_data = r#"{
        "data": [
            {
                "id": "default-workflow",
                "name": "Production Build",
                "source": "file"
            },
            {
                "id": "staging-workflow",
                "name": null,
                "source": "ui"
            }
        ]
    }"#;

    let wrapper: DataWrapper<Vec<Workflow>> = serde_json::from_str(json_data).unwrap();
    assert_eq!(wrapper.data.len(), 2);
    assert_eq!(wrapper.data[0].id, "default-workflow");
    assert_eq!(wrapper.data[0].name.as_deref(), Some("Production Build"));
    assert_eq!(wrapper.data[0].source, "file");
    assert_eq!(wrapper.data[1].id, "staging-workflow");
    assert_eq!(wrapper.data[1].name, None);
    assert_eq!(wrapper.data[1].source, "ui");
}

#[test]
fn test_deserialization_v3_build_with_artifacts() {
    let json_data = r#"{
        "data": {
            "id": "62c2d3e4f56789abcdef0123",
            "app_id": "60a0b1c2d3e4f56789abcdef",
            "index": 42,
            "status": "finished",
            "branch": "main",
            "tag": null,
            "workflow": {
                "id": "release",
                "name": "Release Workflow",
                "source": "file"
            },
            "commit": {
                "hash": "abcdef1234567890",
                "author_name": "Developer",
                "author_email": "dev@example.com",
                "message": "Fix login crash issue",
                "url": "https://github.com/example/repo/commit/abcdef"
            },
            "instance_type": "mac_mini_m2",
            "labels": ["prod", "v1.0"],
            "artifacts": [
                {
                    "name": "app-release.ipa",
                    "size_in_bytes": 45000000,
                    "type": "ipa",
                    "short_lived_download_url": "https://api.codemagic.io/artifacts/abc/app-release.ipa",
                    "version_name": "1.2.0",
                    "version_code": "42"
                }
            ],
            "created_at": "2026-10-15T10:00:00Z",
            "started_at": "2026-10-15T10:01:00Z",
            "finished_at": "2026-10-15T10:15:00Z"
        }
    }"#;

    let wrapper: DataWrapper<Build> = serde_json::from_str(json_data).unwrap();
    let build = wrapper.data;
    assert_eq!(build.id, "62c2d3e4f56789abcdef0123");
    assert_eq!(build.index, Some(42));
    assert_eq!(build.status, "finished");
    assert_eq!(build.instance_type.as_deref(), Some("mac_mini_m2"));
    assert_eq!(build.artifacts.as_ref().unwrap().len(), 1);
    assert_eq!(build.artifacts.as_ref().unwrap()[0].size(), 45000000);
}

#[test]
fn test_deserialization_v3_team_builds_cursor_pagination() {
    let json_data = r#"{
        "data": [
            {
                "id": "6ad0f15212cce3cb996d8f8d",
                "app_id": "6ab52fc4d7518816c76ae734",
                "index": 12,
                "status": "finished",
                "workflow": {
                    "id": "android-release",
                    "name": "Android Release"
                },
                "labels": ["prod"],
                "release_notes": [
                    {
                        "language": "en",
                        "text": "Initial launch"
                    }
                ],
                "created_at": "2026-10-15T12:00:00Z",
                "started_at": "2026-10-15T12:01:00Z",
                "finished_at": "2026-10-15T12:06:00Z",
                "branch": "main",
                "tag": null,
                "commit": null
            }
        ],
        "page_size": 20,
        "cursor": "6ad0f15212cce3cb996d8f8d"
    }"#;

    let paginated: CursorPagination<Build> = serde_json::from_str(json_data).unwrap();
    assert_eq!(paginated.data.len(), 1);
    assert_eq!(paginated.data[0].id, "6ad0f15212cce3cb996d8f8d");
    assert_eq!(
        paginated.data[0].release_notes_summary().as_deref(),
        Some("Initial launch")
    );
    assert_eq!(
        paginated.cursor_str().as_deref(),
        Some("6ad0f15212cce3cb996d8f8d")
    );
}

#[test]
fn test_classic_app_response_deserialization() {
    let json_data = r#"{
        "application": {
            "_id": "60a0b1c2d3e4f56789abcdef",
            "appName": "Legacy App",
            "branches": ["main", "develop", "feature/login"],
            "workflowIds": ["release", "nightly"],
            "workflows": {
                "release": { "name": "Production Release" }
            }
        }
    }"#;

    let res: ClassicAppResponse = serde_json::from_str(json_data).expect("valid json");
    let app: ClassicAppInfo = res.application.expect("app should be present");
    assert_eq!(app.id.as_deref(), Some("60a0b1c2d3e4f56789abcdef"));
    assert_eq!(app.app_name.as_deref(), Some("Legacy App"));
    assert_eq!(app.branches.as_ref().unwrap().len(), 3);
    assert_eq!(app.workflow_ids.as_ref().unwrap().len(), 2);
}

#[test]
fn test_models_request_payload_serialization() {
    // 1. AddAppRequest
    let add_req = AddAppRequest {
        repository_url: "git@github.com:example/app.git".to_string(),
        team_id: Some("team_123".to_string()),
    };
    let json_add = serde_json::to_string(&add_req).unwrap();
    assert!(json_add.contains("repositoryUrl"));
    assert!(json_add.contains("teamId"));

    // 2. StartBuildRequest
    let mut vars = HashMap::new();
    vars.insert("KEY".to_string(), "VALUE".to_string());

    let start_req = StartBuildRequest {
        workflow_id: "release".to_string(),
        branch: Some("main".to_string()),
        tag: None,
        instance_type: Some("mac_mini_m2".to_string()),
        ssh_access_enabled: Some(true),
        labels: Some(vec!["v1".to_string()]),
        environment: Some(BuildEnvironment {
            variables: Some(vars),
            groups: Some(vec!["prod-secrets".to_string()]),
            software_versions: None,
        }),
        inputs: None,
    };
    let json_start = serde_json::to_string(&start_req).unwrap();
    assert!(json_start.contains("workflow_id"));
    assert!(json_start.contains("mac_mini_m2"));
    assert!(json_start.contains("prod-secrets"));

    // 3. CreatePublicUrlRequest & Response
    let pub_req = CreatePublicUrlRequest {
        expires_at: 1792080000,
    };
    let json_pub = serde_json::to_string(&pub_req).unwrap();
    assert!(json_pub.contains("expiresAt"));

    let pub_res: CreatePublicUrlResponse = serde_json::from_str(
        r#"{
        "url": "https://api.codemagic.io/artifacts/pub/app.ipa",
        "expiresAt": "2026-10-16T12:00:00Z"
    }"#,
    )
    .unwrap();
    assert_eq!(
        pub_res.url,
        "https://api.codemagic.io/artifacts/pub/app.ipa"
    );
    assert_eq!(pub_res.expires_at.as_deref(), Some("2026-10-16T12:00:00Z"));

    // 4. User model
    let user: User = serde_json::from_str(
        r#"{
        "id": "usr_999",
        "avatar_url": "https://example.com/avatar.png"
    }"#,
    )
    .unwrap();
    assert_eq!(user.id, "usr_999");
    assert_eq!(
        user.avatar_url.as_deref(),
        Some("https://example.com/avatar.png")
    );
}

#[test]
fn test_error_display_formatting() {
    let unauthorized = CodemagicError::Unauthorized;
    assert!(unauthorized.to_string().contains("401 Unauthorized"));

    let forbidden = CodemagicError::Forbidden("No access to team".to_string());
    assert!(forbidden.to_string().contains("403 Forbidden"));
    assert!(forbidden.to_string().contains("No access to team"));

    let not_found = CodemagicError::NotFound("Build missing".to_string());
    assert!(not_found.to_string().contains("404 Not Found"));

    let rate_limited = CodemagicError::RateLimited;
    assert!(rate_limited.to_string().contains("429 Too Many Requests"));

    let api_err = CodemagicError::ApiError {
        status: 500,
        message: "Internal server error".to_string(),
    };
    assert!(api_err.to_string().contains("500"));
    assert!(api_err.to_string().contains("Internal server error"));

    let missing_token = CodemagicError::MissingToken;
    assert!(missing_token.to_string().contains("No API token provided"));
}

// =============================================================================
// CLI Parsing & Shell Completion Tests
// =============================================================================

#[test]
fn test_cli_parsing_build_start() {
    let cli = Cli::try_parse_from([
        "codemagic",
        "build",
        "start",
        "app_123",
        "--workflow",
        "release",
        "--branch",
        "main",
        "--machine",
        "mac_mini_m2",
        "--wait",
    ])
    .expect("valid cli arguments");

    assert_eq!(cli.output, OutputFormat::Table);
    match cli.command {
        Commands::Build(build_args) => match build_args.command {
            codemagic::cli::build::BuildCommands::Start {
                app_id,
                workflow,
                branch,
                tag,
                machine,
                wait,
                ..
            } => {
                assert_eq!(app_id, "app_123");
                assert_eq!(workflow, "release");
                assert_eq!(branch.as_deref(), Some("main"));
                assert_eq!(tag, None);
                assert_eq!(machine.as_deref(), Some("mac_mini_m2"));
                assert!(wait);
            }
            _ => panic!("unexpected subcommand"),
        },
        _ => panic!("unexpected command"),
    }
}

#[test]
fn test_cli_conflicts_branch_and_tag() {
    let result = Cli::try_parse_from([
        "codemagic",
        "build",
        "start",
        "app_123",
        "--workflow",
        "release",
        "--branch",
        "main",
        "--tag",
        "v1.0.0",
    ]);

    assert!(result.is_err(), "branch and tag must conflict");
}

#[test]
fn test_cli_watch_interval_zero_rejected() {
    let result = Cli::try_parse_from([
        "codemagic",
        "build",
        "watch",
        "build_123",
        "--interval",
        "0",
    ]);
    assert!(
        result.is_err(),
        "--interval 0 must be rejected by value_parser range(1..)"
    );

    let valid = Cli::try_parse_from([
        "codemagic",
        "build",
        "watch",
        "build_123",
        "--interval",
        "1",
    ]);
    assert!(valid.is_ok());
}

#[test]
fn test_cli_parsing_global_flags() {
    let cli = Cli::try_parse_from([
        "codemagic",
        "--token",
        "override_token",
        "-o",
        "json",
        "app",
        "list",
        "--team-id",
        "team_abc",
    ])
    .expect("valid cli flags");

    assert_eq!(cli.token.as_deref(), Some("override_token"));
    assert_eq!(cli.output, OutputFormat::Json);
}

#[test]
fn test_cli_parsing_artifact_commands() {
    let cli = Cli::try_parse_from([
        "codemagic",
        "artifact",
        "download",
        "build_abc",
        "--name",
        "app.apk",
        "--dest",
        "./dist",
    ])
    .expect("valid artifact download arguments");

    match cli.command {
        Commands::Artifact(art_args) => match art_args.command {
            codemagic::cli::artifact::ArtifactCommands::Download {
                build_id,
                name,
                dest,
            } => {
                assert_eq!(build_id, "build_abc");
                assert_eq!(name.as_deref(), Some("app.apk"));
                assert_eq!(dest.unwrap().to_str(), Some("./dist"));
            }
            _ => panic!("unexpected artifact subcommand"),
        },
        _ => panic!("unexpected command"),
    }
}

#[test]
fn test_shell_completions_generation() {
    use clap::CommandFactory;

    for shell in [
        clap_complete::Shell::Bash,
        clap_complete::Shell::Zsh,
        clap_complete::Shell::Fish,
        clap_complete::Shell::PowerShell,
        clap_complete::Shell::Elvish,
    ] {
        let mut cmd = Cli::command();
        let mut buf = Vec::new();
        clap_complete::generate(shell, &mut cmd, "codemagic", &mut buf);
        assert!(!buf.is_empty(), "completion script must not be empty");
        let script = String::from_utf8(buf).expect("valid utf-8 shell script");
        assert!(
            script.contains("codemagic"),
            "completion script must contain binary name"
        );
    }
}

// =============================================================================
// Table Rendering Safety Tests (No Panics, Including Multi-Byte UTF-8)
// =============================================================================

#[test]
fn test_render_apps_table() {
    let apps = vec![
        App {
            id: "60a0b1c2d3e4f56789abcdef".to_string(),
            name: "iOS Production App 🚀".to_string(),
            icon_url: None,
            last_build_id: Some("61b1c2d3e4f56789abcdef01".to_string()),
            archived: Some(false),
        },
        App {
            id: "60a0b1c2d3e4f56789abcdeg".to_string(),
            name: "Android App".to_string(),
            icon_url: None,
            last_build_id: None,
            archived: Some(true),
        },
    ];

    render_apps_table(&apps);
    render_apps_table(&[]);
}

#[test]
fn test_render_all_tables_without_panic() {
    // 1. Teams
    let teams = vec![Team {
        id: "team_1".to_string(),
        name: "Mobile Team".to_string(),
        icon_url: None,
    }];
    render_teams_table(&teams);
    render_teams_table(&[]);

    // 2. Builds (including non-ASCII characters in commit hash to verify char-boundary slicing)
    let builds = vec![Build {
        id: "build_1".to_string(),
        app_id: Some("app_1".to_string()),
        index: Some(10),
        status: "building".to_string(),
        branch: Some("main".to_string()),
        tag: None,
        workflow: Some(WorkflowRef {
            id: "release".to_string(),
            name: Some("Release".to_string()),
            source: Some("file".to_string()),
        }),
        commit: Some(CommitInfo {
            hash: Some("🚀🔥✨🎉abcdef12345".to_string()),
            author_name: Some("Author".to_string()),
            author_email: None,
            message: Some("Commit msg".to_string()),
            url: None,
        }),
        instance_type: Some("linux".to_string()),
        labels: Some(vec!["ci".to_string()]),
        artifacts: None,
        created_at: Some("2026-10-15T12:00:00Z".to_string()),
        started_at: Some("2026-10-15T12:01:00Z".to_string()),
        finished_at: None,
        release_notes: None,
    }];
    render_builds_table(&builds);
    render_builds_table(&[]);
    render_build_detail(&builds[0]);

    // 3. Artifacts
    let artifacts = vec![Artifact {
        name: "app.ipa".to_string(),
        size_in_bytes: Some(25000000),
        artifact_type: Some("ipa".to_string()),
        short_lived_download_url: None,
        version_code: Some("10".to_string()),
        version_name: Some("1.0.0".to_string()),
    }];
    render_artifacts_table(&artifacts);
    render_artifacts_table(&[]);

    // 4. Workflows
    let workflows = vec![Workflow {
        id: "wf_1".to_string(),
        name: Some("Workflow One".to_string()),
        source: "file".to_string(),
    }];
    render_workflows_table(&workflows);
    render_workflows_table(&[]);

    // 5. Variable Groups
    let groups = vec![VariableGroup {
        id: "vg_1".to_string(),
        name: "Production Credentials".to_string(),
    }];
    render_variable_groups_table(&groups);
    render_variable_groups_table(&[]);

    // 6. Variables
    let vars = vec![
        EnvironmentVariable {
            id: "v_1".to_string(),
            name: "API_KEY".to_string(),
            value: None,
            secure: true,
        },
        EnvironmentVariable {
            id: "v_2".to_string(),
            name: "ENVIRONMENT".to_string(),
            value: Some("production".to_string()),
            secure: false,
        },
    ];
    render_variables_table(&vars);
    render_variables_table(&[]);
}

// =============================================================================
// Local Mock HTTP Server & CodemagicClient Integration Tests
// =============================================================================

/// Spawns a single-request local HTTP server returning `status_line`, `extra_headers`, and `body`,
/// and captures the raw incoming HTTP request string for header/path assertions.
async fn spawn_mock_server(
    status_line: &'static str,
    extra_headers: Vec<String>,
    body: Vec<u8>,
) -> (String, tokio::task::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base_url = format!("http://{}", addr);

    let handle = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 4096];
        let n = socket.read(&mut buf).await.unwrap_or(0);
        let raw_req = String::from_utf8_lossy(&buf[..n]).to_string();

        let mut resp = format!(
            "HTTP/1.1 {}\r\nContent-Length: {}\r\nConnection: close\r\n",
            status_line,
            body.len()
        );
        for h in extra_headers {
            resp.push_str(&h);
            resp.push_str("\r\n");
        }
        resp.push_str("\r\n");

        let _ = socket.write_all(resp.as_bytes()).await;
        let _ = socket.write_all(&body).await;
        let _ = socket.flush().await;
        raw_req
    });

    (base_url, handle)
}

#[test]
fn test_client_missing_token_validation() {
    assert!(matches!(
        CodemagicClient::new("".to_string(), None, None),
        Err(CodemagicError::MissingToken)
    ));
    assert!(matches!(
        CodemagicClient::new("   ".to_string(), None, None),
        Err(CodemagicError::MissingToken)
    ));

    let valid = CodemagicClient::new(" my_token ".to_string(), None, None).unwrap();
    assert_eq!(valid.token(), "my_token");
}

#[tokio::test]
async fn test_client_endpoints_and_headers() {
    // 1. get_current_user verifies x-auth-token is sent and Content-Type is NOT sent on GET
    let (base, req_handle) = spawn_mock_server(
        "200 OK",
        vec!["Content-Type: application/json".to_string()],
        br#"{"data":{"id":"user_42","avatar_url":null}}"#.to_vec(),
    )
    .await;

    let client =
        CodemagicClient::new("test_secret_token".to_string(), Some(&base), Some(&base)).unwrap();
    let user = client.get_current_user().await.unwrap();
    assert_eq!(user.id, "user_42");

    let raw_req = req_handle.await.unwrap().to_lowercase();
    assert!(raw_req.starts_with("get /api/v3/user http/1.1"));
    assert!(raw_req.contains("x-auth-token: test_secret_token"));
    assert!(!raw_req.contains("content-type: application/json"));

    // 2. cancel_build: HTTP 200 -> Ok(true), HTTP 208 -> Ok(false)
    let (base_200, _) = spawn_mock_server("200 OK", vec![], b"{}".to_vec()).await;
    let client_200 =
        CodemagicClient::new("tok".to_string(), Some(&base_200), Some(&base_200)).unwrap();
    assert!(client_200.cancel_build("b_1").await.unwrap());

    let (base_208, _) = spawn_mock_server("208 Already Reported", vec![], b"{}".to_vec()).await;
    let client_208 =
        CodemagicClient::new("tok".to_string(), Some(&base_208), Some(&base_208)).unwrap();
    assert!(!client_208.cancel_build("b_1").await.unwrap());

    // 3. list_team_builds with filters
    let (base_builds, builds_req_handle) = spawn_mock_server(
        "200 OK",
        vec![],
        br#"{"data":[],"page_size":10,"cursor":null}"#.to_vec(),
    )
    .await;
    let client_builds =
        CodemagicClient::new("tok".to_string(), Some(&base_builds), Some(&base_builds)).unwrap();
    let filter = ListBuildsFilter {
        page_size: 10,
        cursor: Some("cur_1"),
        app_id: Some("app_1"),
        workflow_id: Some("wf_1"),
        branch: Some("feat/a b"),
        status: Some("finished"),
    };
    let res = client_builds
        .list_team_builds("team_9", &filter)
        .await
        .unwrap();
    assert!(res.data.is_empty());
    let builds_raw_req = builds_req_handle.await.unwrap();
    assert!(builds_raw_req.contains(
        "GET /api/v3/teams/team_9/builds?page_size=10&cursor=cur_1&app_id=app_1&workflow_id=wf_1&branch=feat%2Fa%20b&status=finished"
    ));
}

#[tokio::test]
async fn test_client_error_status_mapping_and_utf8_truncation() {
    // 401 Unauthorized
    let (base_401, _) = spawn_mock_server(
        "401 Unauthorized",
        vec![],
        br#"{"message":"bad token"}"#.to_vec(),
    )
    .await;
    let c_401 = CodemagicClient::new("tok".to_string(), Some(&base_401), None).unwrap();
    assert!(matches!(
        c_401.get_current_user().await,
        Err(CodemagicError::Unauthorized)
    ));

    // 403 Forbidden
    let (base_403, _) = spawn_mock_server(
        "403 Forbidden",
        vec![],
        br#"{"detail":"forbidden team"}"#.to_vec(),
    )
    .await;
    let c_403 = CodemagicClient::new("tok".to_string(), Some(&base_403), None).unwrap();
    assert!(matches!(
        c_403.get_current_user().await,
        Err(CodemagicError::Forbidden(msg)) if msg == "forbidden team"
    ));

    // 404 Not Found
    let (base_404, _) = spawn_mock_server(
        "404 Not Found",
        vec![],
        br#"{"error":"missing build"}"#.to_vec(),
    )
    .await;
    let c_404 = CodemagicClient::new("tok".to_string(), Some(&base_404), None).unwrap();
    assert!(matches!(
        c_404.get_build("b_missing").await,
        Err(CodemagicError::NotFound(msg)) if msg == "missing build"
    ));

    // 429 Rate Limited
    let (base_429, _) = spawn_mock_server("429 Too Many Requests", vec![], b"".to_vec()).await;
    let c_429 = CodemagicClient::new("tok".to_string(), Some(&base_429), None).unwrap();
    assert!(matches!(
        c_429.get_current_user().await,
        Err(CodemagicError::RateLimited)
    ));

    // 200 OK with >300 multi-byte UTF-8 malformed JSON body (must not panic on char boundary!)
    let multi_byte_body = "🚀🔥✨🎉".repeat(120); // 480 chars, 1920 bytes
    let (base_utf8, _) = spawn_mock_server("200 OK", vec![], multi_byte_body.into_bytes()).await;
    let c_utf8 = CodemagicClient::new("tok".to_string(), Some(&base_utf8), None).unwrap();
    let err = c_utf8.get_current_user().await.unwrap_err();
    assert!(err.to_string().contains("Failed to parse response JSON"));
}

#[tokio::test]
async fn test_client_download_file_redirect_strips_auth_header() {
    // 1. Spawn external storage server (simulating S3/GCS presigned target)
    let payload = b"binary_artifact_contents_1234567890".to_vec();
    let (storage_base, storage_req_handle) =
        spawn_mock_server("200 OK", vec![], payload.clone()).await;

    // 2. Spawn Codemagic classic API server that redirects (302) to the external storage server
    let redirect_location = format!("Location: {}/bucket/app.apk", storage_base);
    let (classic_base, cm_req_handle) =
        spawn_mock_server("302 Found", vec![redirect_location], Vec::new()).await;

    let client =
        CodemagicClient::new("super_secret_token".to_string(), None, Some(&classic_base)).unwrap();

    let tmp_file = std::env::temp_dir().join(format!("cm_dl_test_{}.apk", std::process::id()));
    let _ = std::fs::remove_file(&tmp_file);

    let dl_url = format!("{}/artifacts/sec/app.apk", classic_base);
    client
        .download_file(&dl_url, &tmp_file, Some(|_cur, _tot| {}))
        .await
        .expect("download_file should follow redirect and succeed");

    // Verify file contents match
    let saved_bytes = std::fs::read(&tmp_file).unwrap();
    assert_eq!(saved_bytes, payload);
    let _ = std::fs::remove_file(&tmp_file);

    // Verify Codemagic host received x-auth-token
    let cm_req = cm_req_handle.await.unwrap().to_lowercase();
    assert!(cm_req.contains("x-auth-token: super_secret_token"));

    // Verify external storage host did NOT receive x-auth-token!
    let storage_req = storage_req_handle.await.unwrap().to_lowercase();
    assert!(
        !storage_req.contains("x-auth-token"),
        "x-auth-token must never be leaked to external redirect storage hosts"
    );
}
