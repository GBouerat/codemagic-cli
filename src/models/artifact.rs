//! Build artifact models for Codemagic APIs.

use serde::{Deserialize, Serialize};

/// Build artifact metadata returned by `/api/v3/builds/{build_id}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    /// File name of the artifact (e.g. `app-release.apk`, `app.ipa`).
    pub name: String,
    /// Size of the artifact in bytes, if reported.
    #[serde(default)]
    pub size_in_bytes: Option<u64>,
    /// Artifact classification (e.g. `apk`, `ipa`, `mapping`, `log`).
    #[serde(rename = "type")]
    pub artifact_type: Option<String>,
    /// Pre-signed temporary download URL, if available.
    #[serde(default)]
    pub short_lived_download_url: Option<String>,
    /// Application version code / build number extracted from binary.
    pub version_code: Option<String>,
    /// Application marketing version name extracted from binary.
    pub version_name: Option<String>,
}

impl Artifact {
    /// Returns the artifact size in bytes, defaulting to `0` if unspecified.
    pub fn size(&self) -> u64 {
        self.size_in_bytes.unwrap_or(0)
    }
}

/// Request payload to generate a public download URL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreatePublicUrlRequest {
    /// Unix epoch timestamp in seconds when the link should expire.
    #[serde(rename = "expiresAt")]
    pub expires_at: u64,
}

/// Response returned when a public download link is successfully created.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreatePublicUrlResponse {
    /// The public, shareable download URL.
    pub url: String,
    /// Human-readable expiration date string.
    #[serde(rename = "expiresAt")]
    pub expires_at: Option<String>,
}
