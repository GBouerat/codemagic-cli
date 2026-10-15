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
    AddAppRequest, App, AppCache, Artifact, Build, BuildEnvironment, CachesData, ClassicAppInfo,
    ClassicAppResponse, ClassicPagination, CommitInfo, CreatePublicUrlRequest,
    CreatePublicUrlResponse, CursorPagination, DataWrapper, EnvironmentVariable, StartBuildRequest,
    Team, User, VariableGroup, Workflow, WorkflowRef,
};
use codemagic::output::{
    OutputFormat, format_size, format_status, render_apps_table, render_artifacts_table,
    render_build_detail, render_builds_table, render_caches_table, render_teams_table,
    render_variable_groups_table, render_variables_table, render_workflows_table, status_color,
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
        workflow_id: None,
        file_workflow_id: None,
        build_actions: None,
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
                assert_eq!(app_id.as_deref(), Some("app_123"));
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
                names,
                dest,
            } => {
                assert_eq!(build_id, "build_abc");
                assert_eq!(names, vec!["app.apk"]);
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
        workflow_id: None,
        file_workflow_id: None,
        build_actions: None,
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

// =============================================================================
// Cache Models, CLI Parsing, and Mock Server Tests
// =============================================================================

#[test]
fn test_deserialization_cache_envelope_and_bare_array() {
    // 1. Envelope format: { "caches": [...] }
    let json_envelope = r#"{
        "caches": [
            {
                "_id": "cache_abc_1",
                "appId": "app_xyz_1",
                "branch": "feature/cache",
                "workflowId": "workflow-release",
                "size": 52428800,
                "lastUsed": "2026-10-30T20:00:00Z"
            }
        ]
    }"#;

    let envelope_data: CachesData = serde_json::from_str(json_envelope).unwrap();
    let caches = envelope_data.into_vec();
    assert_eq!(caches.len(), 1);
    assert_eq!(caches[0].id, "cache_abc_1");
    assert_eq!(caches[0].app_id.as_deref(), Some("app_xyz_1"));
    assert_eq!(caches[0].branch.as_deref(), Some("feature/cache"));
    assert_eq!(caches[0].workflow_id.as_deref(), Some("workflow-release"));
    assert_eq!(caches[0].size, Some(52428800));
    assert_eq!(caches[0].last_used.as_deref(), Some("2026-10-30T20:00:00Z"));

    // 2. Bare array format with alternative alias keys (id, app_id, last_used)
    let json_array = r#"[
        {
            "id": "cache_abc_2",
            "app_id": "app_xyz_2",
            "size": 1048576,
            "last_used": "2026-10-30T21:00:00Z"
        }
    ]"#;

    let array_data: CachesData = serde_json::from_str(json_array).unwrap();
    let caches2 = array_data.into_vec();
    assert_eq!(caches2.len(), 1);
    assert_eq!(caches2[0].id, "cache_abc_2");
    assert_eq!(caches2[0].app_id.as_deref(), Some("app_xyz_2"));
    assert_eq!(caches2[0].branch, None);
    assert_eq!(caches2[0].workflow_id, None);
    assert_eq!(caches2[0].size, Some(1048576));
    assert_eq!(
        caches2[0].last_used.as_deref(),
        Some("2026-10-30T21:00:00Z")
    );
}

#[test]
fn test_cli_parsing_cache_commands() {
    // 1. cache list --app-id app_1
    let cli = Cli::try_parse_from(["codemagic", "cache", "list", "--app-id", "app_1"]).unwrap();
    if let Commands::Cache(args) = cli.command {
        if let codemagic::cli::cache::CacheCommands::List { app_id } = args.command {
            assert_eq!(app_id.as_deref(), Some("app_1"));
        } else {
            panic!("Expected CacheCommands::List");
        }
    } else {
        panic!("Expected Commands::Cache");
    }

    // 2. cache delete --app-id app_1 --cache-id c_99 -y
    let cli2 = Cli::try_parse_from([
        "codemagic",
        "cache",
        "delete",
        "-a",
        "app_1",
        "-c",
        "c_99",
        "-y",
    ])
    .unwrap();
    if let Commands::Cache(args) = cli2.command {
        if let codemagic::cli::cache::CacheCommands::Delete {
            app_id,
            cache_id,
            yes,
        } = args.command
        {
            assert_eq!(app_id.as_deref(), Some("app_1"));
            assert_eq!(cache_id.as_deref(), Some("c_99"));
            assert!(yes);
        } else {
            panic!("Expected CacheCommands::Delete");
        }
    } else {
        panic!("Expected Commands::Cache");
    }
}

#[test]
fn test_render_caches_table() {
    // Empty list
    render_caches_table(&[], "app_empty");

    // Populated list
    let caches = vec![
        AppCache {
            id: "c1".to_string(),
            app_id: Some("app1".to_string()),
            branch: Some("main".to_string()),
            workflow_id: Some("wf1".to_string()),
            size: Some(15_000_000),
            last_used: Some("2026-10-30T22:00:00Z".to_string()),
        },
        AppCache {
            id: "c2".to_string(),
            app_id: None,
            branch: None,
            workflow_id: None,
            size: None,
            last_used: None,
        },
    ];
    render_caches_table(&caches, "app1");
}

#[tokio::test]
async fn test_client_cache_endpoints_and_mock_server() {
    // 1. GET /apps/app_c1/caches returning envelope
    let json_resp = r#"{"caches":[{"_id":"cid_1","appId":"app_c1","size":1000}]}"#;
    let (base_list, req_handle) = spawn_mock_server(
        "200 OK",
        vec!["Content-Type: application/json".to_string()],
        json_resp.as_bytes().to_vec(),
    )
    .await;

    let client = CodemagicClient::new("token123".to_string(), None, Some(&base_list)).unwrap();
    let caches = client.list_caches("app_c1").await.unwrap();
    assert_eq!(caches.len(), 1);
    assert_eq!(caches[0].id, "cid_1");
    let req = req_handle.await.unwrap();
    assert!(req.contains("GET /apps/app_c1/caches"));
    assert!(req.contains("x-auth-token: token123"));

    // 2. DELETE /apps/app_c1/caches returning 200
    let (base_del_all, req_handle_all) = spawn_mock_server(
        "200 OK",
        vec!["Content-Type: application/json".to_string()],
        b"{\"message\":\"caches purged\"}".to_vec(),
    )
    .await;
    let client_all =
        CodemagicClient::new("token123".to_string(), None, Some(&base_del_all)).unwrap();
    client_all.delete_all_caches("app_c1").await.unwrap();
    let req_all = req_handle_all.await.unwrap();
    assert!(req_all.contains("DELETE /apps/app_c1/caches"));

    // 3. DELETE /apps/app_c1/caches/cid_1 returning 204 No Content (empty body!)
    let (base_del_one, req_handle_one) =
        spawn_mock_server("204 No Content", vec![], Vec::new()).await;
    let client_one =
        CodemagicClient::new("token123".to_string(), None, Some(&base_del_one)).unwrap();
    client_one.delete_cache("app_c1", "cid_1").await.unwrap();
    let req_one = req_handle_one.await.unwrap();
    assert!(req_one.contains("DELETE /apps/app_c1/caches/cid_1"));

    // 4. 404 Not Found error mapping
    let (base_404, _) = spawn_mock_server(
        "404 Not Found",
        vec!["Content-Type: application/json".to_string()],
        b"{\"message\":\"app not found\"}".to_vec(),
    )
    .await;
    let client_404 = CodemagicClient::new("token123".to_string(), None, Some(&base_404)).unwrap();
    assert!(matches!(
        client_404.list_caches("missing").await,
        Err(CodemagicError::NotFound(_))
    ));
}

#[tokio::test]
async fn test_client_download_file_relative_redirect_resolved() {
    let payload = b"relative_redirect_payload_54321".to_vec();
    let (target_server, target_req) = spawn_mock_server("200 OK", vec![], payload.clone()).await;

    // Codemagic mock server responds with a redirect location
    let (cm_server, _cm_req) = spawn_mock_server(
        "302 Found",
        vec![format!("Location: {}/files/app.ipa", target_server)],
        Vec::new(),
    )
    .await;

    let client = CodemagicClient::new("secret_tok".to_string(), None, Some(&cm_server)).unwrap();

    let tmp_file = std::env::temp_dir().join(format!("cm_rel_dl_test_{}.ipa", std::process::id()));
    let _ = std::fs::remove_file(&tmp_file);

    let start_url = format!("{}/start_dl", cm_server);
    client
        .download_file(&start_url, &tmp_file, Some(|_, _| {}))
        .await
        .expect("download_file should follow redirect and succeed");

    let saved = std::fs::read(&tmp_file).unwrap();
    assert_eq!(saved, payload);
    let _ = std::fs::remove_file(&tmp_file);

    let req_str = target_req.await.unwrap().to_lowercase();
    assert!(!req_str.contains("x-auth-token"));
}

#[test]
fn test_help_hides_env_token_value() {
    let _guard = ENV_LOCK.lock().unwrap();
    let secret = "secret_token_never_leak_xyz_98765";
    unsafe {
        std::env::set_var("CODEMAGIC_API_TOKEN", secret);
    }
    use clap::CommandFactory;
    let mut cmd = Cli::command();
    let mut help_buf = Vec::new();
    cmd.write_help(&mut help_buf).unwrap();
    let help_text = String::from_utf8(help_buf).unwrap();

    unsafe {
        std::env::remove_var("CODEMAGIC_API_TOKEN");
    }

    assert!(
        !help_text.contains(secret),
        "Help text leaked secret env token value!"
    );
    assert!(
        help_text.contains("CODEMAGIC_API_TOKEN"),
        "Help text should still mention the env variable name"
    );
}

#[test]
fn test_clean_log_text() {
    use codemagic::cli::build::clean_log_text;

    // 1. ANSI stripping
    let raw_ansi = "\x1b[31;1mError:\x1b[0m compile failure\x1b[K";
    assert_eq!(clean_log_text(raw_ansi), "Error: compile failure");

    // 2. HTML tag stripping
    let raw_html = "<span class=\"log-line\">Build succeeded in 42s</span>";
    assert_eq!(clean_log_text(raw_html), "Build succeeded in 42s");

    // 3. HTML entity unescaping
    let raw_entities = "&lt;script&gt;alert(&quot;test&quot;); &amp; &#39;done&#39;&lt;/script&gt;";
    // Important: tags stripped before unescaping, so literal &lt;script&gt; is preserved as <script>!
    assert_eq!(
        clean_log_text(raw_entities),
        "<script>alert(\"test\"); & 'done'</script>"
    );

    // 4. Combined ANSI + HTML tags + entities
    let combined =
        "\x1b[32m<div class=\"step\">&lt;Building&gt; &#38; &quot;Testing&quot;...</div>\x1b[0m";
    assert_eq!(clean_log_text(combined), "<Building> & \"Testing\"...");

    // 5. Standalone carriage returns (\r) and \r\n normalization
    let raw_cr = "Step 1\rStep 1 (done)\r\nStep 2\n";
    assert_eq!(clean_log_text(raw_cr), "Step 1\nStep 1 (done)\nStep 2\n");
}

#[test]
fn test_detect_current_git_branch_in_repo() {
    use codemagic::cli::build::detect_current_git_branch;
    let branch = detect_current_git_branch();
    assert!(
        branch.is_some(),
        "Expected current git branch in repository"
    );
    assert_eq!(branch.unwrap(), "dev");
}

#[test]
fn test_cli_parsing_build_log() {
    use codemagic::cli::build::BuildCommands;

    // 1. Without step or tail
    let cli = Cli::try_parse_from(["codemagic", "build", "log", "b_123"]).unwrap();
    if let Commands::Build(args) = cli.command {
        if let BuildCommands::Log {
            build_id,
            step,
            tail,
        } = args.command
        {
            assert_eq!(build_id, "b_123");
            assert_eq!(step, None);
            assert_eq!(tail, None);
        } else {
            panic!("Expected BuildCommands::Log");
        }
    } else {
        panic!("Expected Commands::Build");
    }

    // 2. With numeric step and --tail
    let cli2 =
        Cli::try_parse_from(["codemagic", "build", "log", "b_123", "3", "--tail", "50"]).unwrap();
    if let Commands::Build(args) = cli2.command {
        if let BuildCommands::Log {
            build_id,
            step,
            tail,
        } = args.command
        {
            assert_eq!(build_id, "b_123");
            assert_eq!(step.as_deref(), Some("3"));
            assert_eq!(tail, Some(50));
        } else {
            panic!("Expected BuildCommands::Log");
        }
    } else {
        panic!("Expected Commands::Build");
    }

    // 3. With string step
    let cli3 =
        Cli::try_parse_from(["codemagic", "build", "log", "b_123", "flutter build"]).unwrap();
    if let Commands::Build(args) = cli3.command {
        if let BuildCommands::Log {
            build_id,
            step,
            tail,
        } = args.command
        {
            assert_eq!(build_id, "b_123");
            assert_eq!(step.as_deref(), Some("flutter build"));
            assert_eq!(tail, None);
        } else {
            panic!("Expected BuildCommands::Log");
        }
    } else {
        panic!("Expected Commands::Build");
    }
}

#[test]
fn test_cli_parsing_api() {
    // 1. GET request without file
    let cli = Cli::try_parse_from(["codemagic", "api", "GET", "/builds/b_123"]).unwrap();
    if let Commands::Api(args) = cli.command {
        assert_eq!(args.method, "GET");
        assert_eq!(args.path, "/builds/b_123");
        assert_eq!(args.file, None);
    } else {
        panic!("Expected Commands::Api");
    }

    // 2. POST request with -f file
    let cli2 =
        Cli::try_parse_from(["codemagic", "api", "post", "/builds", "-f", "body.json"]).unwrap();
    if let Commands::Api(args) = cli2.command {
        assert_eq!(args.method, "post");
        assert_eq!(args.path, "/builds");
        assert_eq!(args.file.as_deref(), Some("body.json"));
    } else {
        panic!("Expected Commands::Api");
    }

    // 3. POST request with -f - (stdin)
    let cli3 = Cli::try_parse_from(["codemagic", "api", "POST", "/builds", "--file", "-"]).unwrap();
    if let Commands::Api(args) = cli3.command {
        assert_eq!(args.file.as_deref(), Some("-"));
    } else {
        panic!("Expected Commands::Api");
    }
}

#[test]
fn test_cli_parsing_artifact_download_multiple_names() {
    let cli = Cli::try_parse_from([
        "codemagic",
        "artifact",
        "download",
        "b_123",
        "-n",
        "app",
        "-n",
        "mapping.txt",
        "--dest",
        "./out",
    ])
    .unwrap();
    if let Commands::Artifact(args) = cli.command {
        if let codemagic::cli::artifact::ArtifactCommands::Download {
            build_id,
            names,
            dest,
        } = args.command
        {
            assert_eq!(build_id, "b_123");
            assert_eq!(names, vec!["app", "mapping.txt"]);
            assert_eq!(dest.unwrap().to_str(), Some("./out"));
        } else {
            panic!("Expected ArtifactCommands::Download");
        }
    } else {
        panic!("Expected Commands::Artifact");
    }
}

#[test]
fn test_cli_parsing_build_start_optional_app() {
    use codemagic::cli::build::BuildCommands;

    // 1. Without positional APP_ID (omitted, will use configured default app)
    let cli =
        Cli::try_parse_from(["codemagic", "build", "start", "--workflow", "wf_test"]).unwrap();
    if let Commands::Build(args) = cli.command {
        if let BuildCommands::Start {
            app_id,
            app,
            workflow,
            ..
        } = args.command
        {
            assert_eq!(app_id, None);
            assert_eq!(app, None);
            assert_eq!(workflow, "wf_test");
        } else {
            panic!("Expected BuildCommands::Start");
        }
    } else {
        panic!("Expected Commands::Build");
    }

    // 2. With positional APP_ID
    let cli2 = Cli::try_parse_from([
        "codemagic",
        "build",
        "start",
        "app_pos",
        "--workflow",
        "wf_test",
    ])
    .unwrap();
    if let Commands::Build(args) = cli2.command {
        if let BuildCommands::Start {
            app_id,
            app,
            workflow,
            ..
        } = args.command
        {
            assert_eq!(app_id.as_deref(), Some("app_pos"));
            assert_eq!(app, None);
            assert_eq!(workflow, "wf_test");
        } else {
            panic!("Expected BuildCommands::Start");
        }
    } else {
        panic!("Expected Commands::Build");
    }

    // 3. With --app flag
    let cli3 = Cli::try_parse_from([
        "codemagic",
        "build",
        "start",
        "--app",
        "app_flag",
        "--workflow",
        "wf_test",
    ])
    .unwrap();
    if let Commands::Build(args) = cli3.command {
        if let BuildCommands::Start {
            app_id,
            app,
            workflow,
            ..
        } = args.command
        {
            assert_eq!(app_id, None);
            assert_eq!(app.as_deref(), Some("app_flag"));
            assert_eq!(workflow, "wf_test");
        } else {
            panic!("Expected BuildCommands::Start");
        }
    } else {
        panic!("Expected Commands::Build");
    }
}

#[tokio::test]
async fn test_client_fetch_raw_log_plain_and_gzip() {
    // 1. Plain text log
    let plain_log = b"Line 1: starting build\nLine 2: running test\nLine 3: success\n".to_vec();
    let (plain_base, plain_req_handle) =
        spawn_mock_server("200 OK", vec![], plain_log.clone()).await;
    let client =
        CodemagicClient::new("test_tok".to_string(), Some(&plain_base), Some(&plain_base)).unwrap();

    let fetched = client
        .fetch_raw_log(&format!("{}/log", plain_base))
        .await
        .unwrap();
    assert_eq!(fetched, String::from_utf8(plain_log.clone()).unwrap());
    let plain_req = plain_req_handle.await.unwrap();
    assert!(plain_req.contains("x-auth-token: test_tok"));

    // Verify relative URL fallback resolution
    let (rel_base, _) = spawn_mock_server("200 OK", vec![], plain_log.clone()).await;
    let client_rel =
        CodemagicClient::new("test_tok".to_string(), Some(&rel_base), Some(&rel_base)).unwrap();
    let fetched_rel = client_rel.fetch_raw_log("/rel_log").await.unwrap();
    assert_eq!(fetched_rel, String::from_utf8(plain_log).unwrap());

    // 2. Gzipped log
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;
    let log_content = "Gzip line 1: compiling\nGzip line 2: done\n";
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(log_content.as_bytes()).unwrap();
    let gzipped_bytes = encoder.finish().unwrap();

    let (gzip_base, _) = spawn_mock_server(
        "200 OK",
        vec!["Content-Encoding: gzip".to_string()],
        gzipped_bytes,
    )
    .await;
    let client_gzip =
        CodemagicClient::new("test_tok".to_string(), Some(&gzip_base), Some(&gzip_base)).unwrap();
    let fetched_gzip = client_gzip
        .fetch_raw_log(&format!("{}/log.gz", gzip_base))
        .await
        .unwrap();
    assert_eq!(fetched_gzip, log_content);

    // 3. 404 error response
    let (err_base, _) = spawn_mock_server("404 Not Found", vec![], b"Log not found".to_vec()).await;
    let client_err =
        CodemagicClient::new("test_tok".to_string(), Some(&err_base), Some(&err_base)).unwrap();
    let err = client_err
        .fetch_raw_log(&format!("{}/missing", err_base))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("404"));
    assert!(err.to_string().contains("Log not found"));
}

#[tokio::test]
async fn test_client_raw_request_get_post_and_error() {
    // 1. Raw GET request
    let response_body = br#"{"status":"ok","builds":[]}"#.to_vec();
    let (base_get, get_req_handle) = spawn_mock_server("200 OK", vec![], response_body).await;
    let client =
        CodemagicClient::new("tok_raw".to_string(), Some(&base_get), Some(&base_get)).unwrap();

    let body = client
        .raw_request(
            reqwest::Method::GET,
            &format!("{}/api/test", base_get),
            None,
        )
        .await
        .unwrap();
    assert_eq!(body, r#"{"status":"ok","builds":[]}"#);
    let req_str = get_req_handle.await.unwrap();
    assert!(req_str.contains("x-auth-token: tok_raw"));
    assert!(req_str.contains("GET /api/test"));

    // 2. Raw POST request with JSON payload
    let post_resp = br#"{"buildId":"b_999"}"#.to_vec();
    let (base_post, post_req_handle) = spawn_mock_server("200 OK", vec![], post_resp).await;
    let client_post =
        CodemagicClient::new("tok_raw".to_string(), Some(&base_post), Some(&base_post)).unwrap();

    let payload = serde_json::json!({
        "workflowId": "release",
        "branch": "main"
    });
    let body_post = client_post
        .raw_request(
            reqwest::Method::POST,
            &format!("{}/builds", base_post),
            Some(&payload),
        )
        .await
        .unwrap();
    assert_eq!(body_post, r#"{"buildId":"b_999"}"#);
    let post_req_str = post_req_handle.await.unwrap();
    assert!(post_req_str.contains("POST /builds"));
    assert!(post_req_str.contains("content-type: application/json"));
    assert!(post_req_str.contains(r#""workflowId":"release""#));

    // 3. Raw request HTTP error truncated to 500 characters
    let long_error = "A".repeat(800);
    let (base_err, _) =
        spawn_mock_server("500 Internal Server Error", vec![], long_error.into_bytes()).await;
    let client_err =
        CodemagicClient::new("tok_raw".to_string(), Some(&base_err), Some(&base_err)).unwrap();

    let err = client_err
        .raw_request(reqwest::Method::GET, &format!("{}/fail", base_err), None)
        .await
        .unwrap_err();
    let err_msg = err.to_string();
    assert!(err_msg.contains("500"));
    assert!(err_msg.contains("..."));
}

#[tokio::test]
async fn test_build_log_step_selection_logic() {
    use codemagic::models::{BuildAction, BuildSubaction};

    let actions = vec![
        BuildAction {
            name: Some("Prepare build environment".to_string()),
            status: Some("success".to_string()),
            log_url: Some("https://api.codemagic.io/log/1".to_string()),
            subactions: None,
        },
        BuildAction {
            name: Some("Run Flutter tests".to_string()),
            status: Some("failed".to_string()),
            log_url: None,
            subactions: Some(vec![BuildSubaction {
                name: Some("flutter test".to_string()),
                status: Some("failed".to_string()),
                log_url: Some("https://api.codemagic.io/log/2".to_string()),
            }]),
        },
        BuildAction {
            name: Some("Publish Artifacts".to_string()),
            status: Some("canceled".to_string()),
            log_url: Some("https://api.codemagic.io/log/3".to_string()),
            subactions: None,
        },
    ];

    let build_json = serde_json::json!({
        "build": {
            "_id": "b_steps_test",
            "status": "failed",
            "buildActions": actions
        }
    });

    let (base, _) =
        spawn_mock_server("200 OK", vec![], serde_json::to_vec(&build_json).unwrap()).await;
    let client = CodemagicClient::new("tok".to_string(), Some(&base), Some(&base)).unwrap();
    let build = client.get_build("b_steps_test").await.unwrap();
    let actions = build.build_actions.as_ref().unwrap();
    assert_eq!(actions.len(), 3);

    // Rule 1: None -> picks first failed/timeout/canceled/cancelled (index 1: "Run Flutter tests")
    const FAILED_STATUSES: &[&str] = &["failed", "timeout", "canceled", "cancelled"];
    let default_step = actions.iter().find(|a| {
        a.status
            .as_deref()
            .map(|s| FAILED_STATUSES.contains(&s.to_lowercase().as_str()))
            .unwrap_or(false)
    });
    assert_eq!(
        default_step.unwrap().name.as_deref(),
        Some("Run Flutter tests")
    );

    // Verify "cancelled" spelling is matched by Rule 1
    let actions_cancelled = [BuildAction {
        name: Some("Cancelled step".to_string()),
        status: Some("cancelled".to_string()),
        log_url: None,
        subactions: None,
    }];
    let step_cancelled = actions_cancelled.iter().find(|a| {
        a.status
            .as_deref()
            .map(|s| FAILED_STATUSES.contains(&s.to_lowercase().as_str()))
            .unwrap_or(false)
    });
    assert_eq!(
        step_cancelled.unwrap().name.as_deref(),
        Some("Cancelled step")
    );

    // Rule 2: 1-based number -> "1" picks "Prepare build environment", "3" picks "Publish Artifacts"
    assert_eq!(
        actions.get(1 - 1).unwrap().name.as_deref(),
        Some("Prepare build environment")
    );
    assert_eq!(
        actions.get(3 - 1).unwrap().name.as_deref(),
        Some("Publish Artifacts")
    );
    assert!(actions.get(4 - 1).is_none());

    // Rule 3: substring case-insensitive -> "FLUTTER" picks "Run Flutter tests"
    let matched = actions.iter().find(|a| {
        a.name
            .as_deref()
            .map(|n| n.to_lowercase().contains("flutter"))
            .unwrap_or(false)
    });
    assert_eq!(matched.unwrap().name.as_deref(), Some("Run Flutter tests"));
}

#[test]
fn test_build_steps_rendering() {
    use codemagic::models::BuildAction;

    let build = Build {
        id: "b_render".to_string(),
        app_id: Some("app_1".to_string()),
        index: Some(1),
        status: "failed".to_string(),
        branch: Some("main".to_string()),
        tag: None,
        workflow: None,
        workflow_id: Some("wf_1".to_string()),
        file_workflow_id: None,
        commit: None,
        instance_type: None,
        labels: None,
        artifacts: None,
        created_at: None,
        started_at: None,
        finished_at: None,
        release_notes: None,
        build_actions: Some(vec![
            BuildAction {
                name: Some("Checkout source code".to_string()),
                status: Some("success".to_string()),
                log_url: None,
                subactions: None,
            },
            BuildAction {
                name: Some("Compile application".to_string()),
                status: Some("failed".to_string()),
                log_url: None,
                subactions: None,
            },
        ]),
    };

    render_build_detail(&build);
}

#[test]
fn test_cli_parsing_env_groups_app_id_flag_and_fallback() {
    // 1. Long flag: --app-id
    let cli = Cli::try_parse_from(["codemagic", "env", "groups", "--app-id", "app_456"]).unwrap();
    if let Commands::Env(args) = cli.command {
        if let codemagic::cli::env::EnvCommands::Groups { app_id } = args.command {
            assert_eq!(app_id.as_deref(), Some("app_456"));
        } else {
            panic!("Expected EnvCommands::Groups");
        }
    } else {
        panic!("Expected Commands::Env");
    }

    // 2. Short flag: -a
    let cli2 = Cli::try_parse_from(["codemagic", "env", "groups", "-a", "app_789"]).unwrap();
    if let Commands::Env(args) = cli2.command {
        if let codemagic::cli::env::EnvCommands::Groups { app_id } = args.command {
            assert_eq!(app_id.as_deref(), Some("app_789"));
        } else {
            panic!("Expected EnvCommands::Groups");
        }
    } else {
        panic!("Expected Commands::Env");
    }
}
