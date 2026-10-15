//! User and team models for Codemagic APIs.

use serde::{Deserialize, Serialize};

/// Authenticated user profile information returned by `GET /api/v3/user`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    /// Unique user ID.
    pub id: String,
    /// User avatar image URL, if set.
    pub avatar_url: Option<String>,
}

/// Team representation returned by `GET /api/v3/user/teams`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Team {
    /// Unique team ID.
    pub id: String,
    /// Human-readable team name.
    pub name: String,
    /// Team logo or icon URL, if set.
    pub icon_url: Option<String>,
}
