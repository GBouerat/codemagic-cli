//! Data models representing Codemagic application cache schemas.
//!
//! Codemagic allows applications to store build and dependency caches (e.g., CocoaPods,
//! Gradle, pub, or Carthage caches) across build runs. This module models the cache
//! entities and API response envelopes returned by the Codemagic Caches API.

use serde::{Deserialize, Serialize};

/// Represents a single stored application cache.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppCache {
    /// Unique identifier of the cache entry.
    #[serde(rename = "_id", alias = "id", default)]
    pub id: String,

    /// Application ID associated with this cache.
    #[serde(rename = "appId", alias = "app_id", default)]
    pub app_id: Option<String>,

    /// Git branch associated with this cache entry, if applicable.
    #[serde(default)]
    pub branch: Option<String>,

    /// Workflow identifier that produced or utilizes this cache.
    #[serde(rename = "workflowId", alias = "workflow_id", default)]
    pub workflow_id: Option<String>,

    /// Size of the cache archive in bytes, if reported.
    #[serde(default)]
    pub size: Option<u64>,

    /// ISO 8601 timestamp indicating when the cache was last read or updated.
    #[serde(rename = "lastUsed", alias = "last_used", default)]
    pub last_used: Option<String>,
}

/// Response envelope for the list caches endpoint (`GET /apps/:id/caches`).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CachesResponse {
    /// List of application caches.
    #[serde(default)]
    pub caches: Vec<AppCache>,
}

/// Helper enum to deserialize either `{ "caches": [...] }` or bare `[...]` arrays.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CachesData {
    /// Envelope format containing `caches` field.
    Envelope(CachesResponse),
    /// Direct array of cache items.
    List(Vec<AppCache>),
}

impl CachesData {
    /// Consumes the wrapper and returns the underlying list of [`AppCache`] records.
    pub fn into_vec(self) -> Vec<AppCache> {
        match self {
            Self::Envelope(env) => env.caches,
            Self::List(list) => list,
        }
    }
}
