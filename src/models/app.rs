//! Application and workflow data models for Codemagic APIs.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Application summary returned by `/api/v3/user/apps` and `/api/v3/teams/{id}/apps`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct App {
    /// Unique 24-character hexadecimal application ID.
    pub id: String,
    /// Human-readable application name.
    pub name: String,
    /// URL to the app icon image, if configured.
    pub icon_url: Option<String>,
    /// Build ID of the most recent build executed for this app.
    pub last_build_id: Option<String>,
    /// Whether the application has been archived.
    pub archived: Option<bool>,
}

/// Workflow object returned by `/api/v3/apps/{app_id}/workflows`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workflow {
    /// Workflow identifier or name.
    pub id: String,
    /// Human-friendly workflow display name.
    pub name: Option<String>,
    /// Source where the workflow was defined (e.g. `"file"` for codemagic.yaml or `"ui"`).
    pub source: String,
}

/// Variable group object returned by `/api/v3/apps/{app_id}/variable-groups`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariableGroup {
    /// Unique variable group ID.
    pub id: String,
    /// Name of the variable group.
    pub name: String,
}

/// Environment variable definition inside a variable group.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvironmentVariable {
    /// Unique variable ID.
    pub id: String,
    /// Variable name / key.
    pub name: String,
    /// Plaintext value if not secret; `None` if hidden.
    pub value: Option<String>,
    /// Whether the variable is marked as secure/secret.
    pub secure: bool,
}

/// Classic workflow information embedded within legacy application responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassicWorkflowInfo {
    /// Display name of the workflow.
    pub name: Option<String>,
}

/// Legacy application metadata returned by `GET https://api.codemagic.io/apps/:id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassicAppInfo {
    /// Application ID (`_id`).
    #[serde(rename = "_id")]
    pub id: Option<String>,
    /// Application display name.
    #[serde(rename = "appName")]
    pub app_name: Option<String>,
    /// Git branches discovered in the repository.
    pub branches: Option<Vec<String>>,
    /// List of workflow identifiers.
    #[serde(rename = "workflowIds")]
    pub workflow_ids: Option<Vec<String>>,
    /// Map of workflow identifiers to details.
    pub workflows: Option<HashMap<String, ClassicWorkflowInfo>>,
}

/// Response wrapper for legacy application details.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassicAppResponse {
    /// The application details object.
    pub application: Option<ClassicAppInfo>,
}

/// Request payload to register a new Git repository as an application.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddAppRequest {
    /// Git repository clone URL (SSH or HTTPS).
    #[serde(rename = "repositoryUrl")]
    pub repository_url: String,
    /// Team ID under which the app should be created.
    #[serde(rename = "teamId", skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,
}
