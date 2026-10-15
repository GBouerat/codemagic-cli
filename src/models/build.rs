//! Build models, trigger requests, and pagination structures for Codemagic APIs.

use super::artifact::Artifact;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Commit metadata associated with a build.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitInfo {
    /// Git commit SHA hash.
    pub hash: Option<String>,
    /// Git author display name.
    pub author_name: Option<String>,
    /// Git author email address.
    pub author_email: Option<String>,
    /// Full Git commit message.
    pub message: Option<String>,
    /// Web URL to view the commit in the source repository hosting service.
    pub url: Option<String>,
}

/// Workflow reference within a build response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowRef {
    /// Workflow identifier or name.
    pub id: String,
    /// Workflow human-friendly name.
    pub name: Option<String>,
    /// Workflow definition source (e.g. `"file"` or `"ui"`).
    pub source: Option<String>,
}

/// Full build model used across Codemagic v3 endpoints.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Build {
    /// Unique 24-character hexadecimal build ID.
    pub id: String,
    /// Application ID this build belongs to.
    pub app_id: Option<String>,
    /// Sequential build index / number for the app.
    pub index: Option<u64>,
    /// Current build status (e.g. `queued`, `preparing`, `building`, `finished`, `failed`, `canceled`).
    pub status: String,
    /// Git branch built.
    pub branch: Option<String>,
    /// Git tag built.
    pub tag: Option<String>,
    /// Workflow executed for this build.
    pub workflow: Option<WorkflowRef>,
    /// Commit information built.
    pub commit: Option<CommitInfo>,
    /// Virtual machine instance type utilized.
    pub instance_type: Option<String>,
    /// Labels assigned to this build.
    pub labels: Option<Vec<String>>,
    /// List of build artifacts produced.
    pub artifacts: Option<Vec<Artifact>>,
    /// ISO 8601 timestamp when the build was queued.
    pub created_at: Option<String>,
    /// ISO 8601 timestamp when build execution began.
    pub started_at: Option<String>,
    /// ISO 8601 timestamp when build execution completed.
    pub finished_at: Option<String>,
    /// Raw release notes, polymorphic across string and array representations.
    #[serde(default)]
    pub release_notes: Option<serde_json::Value>,
}

impl Build {
    /// Extracts a user-friendly summary of release notes regardless of whether
    /// the API returns a string or an array of objects `[{"language": "...", "text": "..."}]`.
    pub fn release_notes_summary(&self) -> Option<String> {
        match &self.release_notes {
            Some(serde_json::Value::String(s)) => {
                if s.trim().is_empty() {
                    None
                } else {
                    Some(s.clone())
                }
            }
            Some(serde_json::Value::Array(arr)) => {
                let texts: Vec<String> = arr
                    .iter()
                    .filter_map(|item| {
                        item.get("text")
                            .or_else(|| item.get("note"))
                            .and_then(|t| t.as_str())
                            .map(|s| s.to_string())
                    })
                    .collect();
                if texts.is_empty() {
                    None
                } else {
                    Some(texts.join("\n"))
                }
            }
            _ => None,
        }
    }
}

/// Response returned when a build is successfully scheduled.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartedBuild {
    /// The unique build ID of the triggered build.
    pub id: String,
}

/// Environment customization passed to [`StartBuildRequest`].
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BuildEnvironment {
    /// Custom key-value environment variables injected into the build machine.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variables: Option<HashMap<String, String>>,
    /// Variable group names to include in the build environment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub groups: Option<Vec<String>>,
    /// Software version overrides (e.g. Flutter, Xcode, Node version).
    #[serde(rename = "softwareVersions", skip_serializing_if = "Option::is_none")]
    pub software_versions: Option<HashMap<String, String>>,
}

/// Payload for triggering a build via `POST /api/v3/apps/{app_id}/builds`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartBuildRequest {
    /// Workflow ID or workflow name.
    pub workflow_id: String,
    /// Branch to build (mutually exclusive with `tag`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Tag to build (mutually exclusive with `branch`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    /// Virtual machine instance type.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance_type: Option<String>,
    /// Whether remote SSH access to the builder should be enabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ssh_access_enabled: Option<bool>,
    /// Custom labels to tag the build with.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub labels: Option<Vec<String>>,
    /// Custom environment variable configurations.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<BuildEnvironment>,
    /// Custom inputs map.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inputs: Option<HashMap<String, serde_json::Value>>,
}

/// Cursor pagination wrapper for team builds list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CursorPagination<T> {
    /// Data items for the current cursor page.
    pub data: Vec<T>,
    /// Maximum page size requested.
    #[serde(default)]
    pub page_size: Option<u32>,
    /// Cursor identifier for fetching the next page.
    #[serde(default)]
    pub cursor: Option<serde_json::Value>,
}

impl<T> CursorPagination<T> {
    /// Extracts a string representation of the cursor token regardless of whether
    /// the backend returns a raw string or a MongoDB-style `{"$oid": "..."}` object.
    pub fn cursor_str(&self) -> Option<String> {
        match &self.cursor {
            Some(serde_json::Value::String(s)) => Some(s.clone()),
            Some(serde_json::Value::Object(map)) => map
                .get("$oid")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            _ => None,
        }
    }
}
