//! Common pagination and wrapper models for Codemagic API responses.

use serde::{Deserialize, Serialize};

/// Generic single-object wrapper returned by Codemagic v3 APIs (`{"data": ...}`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataWrapper<T> {
    /// Wrapped payload object.
    pub data: T,
}

/// Generic page-number pagination wrapper used across Codemagic v3 collection endpoints.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassicPagination<T> {
    /// List of items for the requested page.
    pub data: Vec<T>,
    /// Current 1-based page index.
    pub current_page: u32,
    /// Number of items per page.
    pub page_size: u32,
    /// Total number of pages available.
    pub total_pages: u32,
}
