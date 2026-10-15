//! Codemagic API client implementation.
//!
//! Provides the [`CodemagicClient`] struct, handling authentication, routing
//! across Codemagic REST API v3 and Classic endpoints, cursor-based pagination,
//! error mapping, and streaming downloads.

pub mod error;

use error::{CodemagicError, Result};
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, LOCATION, USER_AGENT};
use std::path::Path;
use std::time::Duration;
use tokio::fs::File;
use tokio::io::AsyncWriteExt;

use crate::models::{
    AddAppRequest, App, Build, ClassicAppResponse, ClassicPagination, CreatePublicUrlRequest,
    CreatePublicUrlResponse, CursorPagination, DataWrapper, EnvironmentVariable, StartBuildRequest,
    StartedBuild, Team, User, VariableGroup, Workflow,
};

/// Default host for Codemagic REST API v3.
const DEFAULT_V3_BASE: &str = "https://codemagic.io";

/// Default host for Codemagic classic API.
const DEFAULT_CLASSIC_BASE: &str = "https://api.codemagic.io";

/// HTTP client for interacting with the Codemagic CI/CD API.
///
/// Handles dual-base URL routing across REST API v3 (`https://codemagic.io/api/v3`)
/// and the classic API (`https://api.codemagic.io`), injecting required `x-auth-token`
/// headers on API requests and providing a clean client for presigned downloads.
#[derive(Clone)]
pub struct CodemagicClient {
    /// Authenticated HTTP client for Codemagic REST API endpoints.
    client: reqwest::Client,
    /// Authenticated HTTP client with redirects disabled to inspect redirect targets before forwarding.
    no_redirect_client: reqwest::Client,
    /// Unauthenticated HTTP client for downloading external/presigned artifact URLs.
    download_client: reqwest::Client,
    /// Resolved API token.
    api_token: String,
    /// Base URL for v3 endpoints.
    v3_base: String,
    /// Base URL for classic endpoints.
    classic_base: String,
}

/// Filter criteria for listing team builds.
#[derive(Debug, Clone, Default)]
pub struct ListBuildsFilter<'a> {
    /// Maximum number of builds to return per page.
    pub page_size: u32,
    /// Cursor for cursor-based pagination.
    pub cursor: Option<&'a str>,
    /// Filter builds by application ID.
    pub app_id: Option<&'a str>,
    /// Filter builds by workflow ID.
    pub workflow_id: Option<&'a str>,
    /// Filter builds by Git branch name.
    pub branch: Option<&'a str>,
    /// Filter builds by status (e.g. `queued`, `building`, `finished`, `failed`).
    pub status: Option<&'a str>,
}

impl CodemagicClient {
    /// Creates a new [`CodemagicClient`] instance.
    ///
    /// # Arguments
    ///
    /// * `api_token` - The Codemagic personal API token.
    /// * `v3_base` - Optional override for the REST v3 base URL (defaults to `https://codemagic.io`).
    /// * `classic_base` - Optional override for the classic base URL (defaults to `https://api.codemagic.io`).
    ///
    /// # Errors
    ///
    /// Returns [`CodemagicError::MissingToken`] if `api_token` is empty or whitespace.
    pub fn new(
        api_token: String,
        v3_base: Option<&str>,
        classic_base: Option<&str>,
    ) -> Result<Self> {
        if api_token.trim().is_empty() {
            return Err(CodemagicError::MissingToken);
        }

        let user_agent =
            HeaderValue::from_static(concat!("codemagic-cli/", env!("CARGO_PKG_VERSION")));

        let mut headers = HeaderMap::new();
        let mut token_val =
            HeaderValue::from_str(api_token.trim()).map_err(|_| CodemagicError::ApiError {
                status: 400,
                message: "Invalid characters in API token".to_string(),
            })?;
        token_val.set_sensitive(true);
        headers.insert("x-auth-token", token_val);
        headers.insert(USER_AGENT, user_agent.clone());

        let client = reqwest::Client::builder()
            .default_headers(headers.clone())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()?;

        let no_redirect_client = reqwest::Client::builder()
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(15))
            .build()?;

        let mut dl_headers = HeaderMap::new();
        dl_headers.insert(USER_AGENT, user_agent);

        let download_client = reqwest::Client::builder()
            .default_headers(dl_headers)
            .connect_timeout(Duration::from_secs(15))
            .build()?;

        Ok(Self {
            client,
            no_redirect_client,
            download_client,
            api_token: api_token.trim().to_string(),
            v3_base: v3_base
                .unwrap_or(DEFAULT_V3_BASE)
                .trim_end_matches('/')
                .to_string(),
            classic_base: classic_base
                .unwrap_or(DEFAULT_CLASSIC_BASE)
                .trim_end_matches('/')
                .to_string(),
        })
    }

    /// Returns a reference to the active API token.
    pub fn token(&self) -> &str {
        &self.api_token
    }

    /// Internal helper to decode and deserialize HTTP responses, mapping HTTP error status codes.
    ///
    /// Truncates long error response snippets safely along Unicode scalar value boundaries.
    async fn handle_response<T: serde::de::DeserializeOwned>(
        &self,
        resp: reqwest::Response,
    ) -> Result<T> {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if status.is_success() {
            match serde_json::from_str::<T>(&text) {
                Ok(data) => Ok(data),
                Err(err) => {
                    let snippet = if text.chars().count() > 300 {
                        let truncated: String = text.chars().take(300).collect();
                        format!("{}...", truncated)
                    } else {
                        text
                    };
                    Err(CodemagicError::ApiError {
                        status: status.as_u16(),
                        message: format!(
                            "Failed to parse response JSON: {}. Body: {}",
                            err, snippet
                        ),
                    })
                }
            }
        } else {
            let status_code = status.as_u16();
            let error_msg = if let Ok(val) = serde_json::from_str::<serde_json::Value>(&text) {
                val.get("detail")
                    .or_else(|| val.get("message"))
                    .or_else(|| val.get("error"))
                    .and_then(|v| v.as_str())
                    .unwrap_or(&text)
                    .to_string()
            } else {
                text
            };

            match status_code {
                401 => Err(CodemagicError::Unauthorized),
                403 => Err(CodemagicError::Forbidden(error_msg)),
                404 => Err(CodemagicError::NotFound(error_msg)),
                429 => Err(CodemagicError::RateLimited),
                _ => Err(CodemagicError::ApiError {
                    status: status_code,
                    message: error_msg,
                }),
            }
        }
    }

    // -------------------------------------------------------------------------
    // User & Teams API
    // -------------------------------------------------------------------------

    /// Retrieves details of the authenticated user: `GET /api/v3/user`.
    pub async fn get_current_user(&self) -> Result<User> {
        let url = format!("{}/api/v3/user", self.v3_base);
        let resp = self.client.get(&url).send().await?;
        let wrapper: DataWrapper<User> = self.handle_response(resp).await?;
        Ok(wrapper.data)
    }

    /// Lists teams accessible to the current user: `GET /api/v3/user/teams`.
    pub async fn get_user_teams(&self) -> Result<Vec<Team>> {
        let url = format!("{}/api/v3/user/teams", self.v3_base);
        let resp = self.client.get(&url).send().await?;
        let paginated: ClassicPagination<Team> = self.handle_response(resp).await?;
        Ok(paginated.data)
    }

    // -------------------------------------------------------------------------
    // Applications API
    // -------------------------------------------------------------------------

    /// Lists applications belonging to the user: `GET /api/v3/user/apps`.
    pub async fn list_user_apps(
        &self,
        page: u32,
        page_size: u32,
        name_filter: Option<&str>,
    ) -> Result<ClassicPagination<App>> {
        let mut url = format!(
            "{}/api/v3/user/apps?page={}&page_size={}",
            self.v3_base, page, page_size
        );
        if let Some(name) = name_filter {
            url.push_str(&format!("&name={}", url_encode(name)));
        }
        let resp = self.client.get(&url).send().await?;
        self.handle_response(resp).await
    }

    /// Lists applications belonging to a team: `GET /api/v3/teams/{team_id}/apps`.
    pub async fn list_team_apps(
        &self,
        team_id: &str,
        page: u32,
        page_size: u32,
        name_filter: Option<&str>,
    ) -> Result<ClassicPagination<App>> {
        let mut url = format!(
            "{}/api/v3/teams/{}/apps?page={}&page_size={}",
            self.v3_base, team_id, page, page_size
        );
        if let Some(name) = name_filter {
            url.push_str(&format!("&name={}", url_encode(name)));
        }
        let resp = self.client.get(&url).send().await?;
        self.handle_response(resp).await
    }

    /// Retrieves legacy application metadata (including branches): `GET https://api.codemagic.io/apps/:id`.
    pub async fn get_classic_app(
        &self,
        app_id: &str,
    ) -> Result<Option<crate::models::ClassicAppInfo>> {
        let url = format!("{}/apps/{}", self.classic_base, app_id);
        let resp = self.client.get(&url).send().await?;
        let res: ClassicAppResponse = self.handle_response(resp).await?;
        Ok(res.application)
    }

    /// Registers a new Git repository as an application: `POST https://api.codemagic.io/apps`.
    pub async fn add_app(&self, req: &AddAppRequest) -> Result<serde_json::Value> {
        let url = format!("{}/apps", self.classic_base);
        let resp = self.client.post(&url).json(req).send().await?;
        self.handle_response(resp).await
    }

    /// Lists workflows configured for an application: `GET /api/v3/apps/{app_id}/workflows`.
    pub async fn list_workflows(&self, app_id: &str) -> Result<Vec<Workflow>> {
        let url = format!("{}/api/v3/apps/{}/workflows", self.v3_base, app_id);
        let resp = self.client.get(&url).send().await?;
        let wrapper: DataWrapper<Vec<Workflow>> = self.handle_response(resp).await?;
        Ok(wrapper.data)
    }

    /// Lists variable groups configured for an application: `GET /api/v3/apps/{app_id}/variable-groups`.
    pub async fn list_variable_groups(&self, app_id: &str) -> Result<Vec<VariableGroup>> {
        let url = format!("{}/api/v3/apps/{}/variable-groups", self.v3_base, app_id);
        let resp = self.client.get(&url).send().await?;
        let paginated: ClassicPagination<VariableGroup> = self.handle_response(resp).await?;
        Ok(paginated.data)
    }

    /// Lists environment variables inside a variable group: `GET /api/v3/variable-groups/{group_id}/variables`.
    pub async fn list_group_variables(&self, group_id: &str) -> Result<Vec<EnvironmentVariable>> {
        let url = format!(
            "{}/api/v3/variable-groups/{}/variables",
            self.v3_base, group_id
        );
        let resp = self.client.get(&url).send().await?;
        let paginated: ClassicPagination<EnvironmentVariable> = self.handle_response(resp).await?;
        Ok(paginated.data)
    }

    // -------------------------------------------------------------------------
    // Builds API
    // -------------------------------------------------------------------------

    /// Lists builds for a team with cursor-based pagination and optional filtering: `GET /api/v3/teams/{team_id}/builds`.
    pub async fn list_team_builds(
        &self,
        team_id: &str,
        filter: &ListBuildsFilter<'_>,
    ) -> Result<CursorPagination<Build>> {
        let mut url = format!(
            "{}/api/v3/teams/{}/builds?page_size={}",
            self.v3_base, team_id, filter.page_size
        );
        if let Some(c) = filter.cursor {
            url.push_str(&format!("&cursor={}", url_encode(c)));
        }
        if let Some(a) = filter.app_id {
            url.push_str(&format!("&app_id={}", url_encode(a)));
        }
        if let Some(w) = filter.workflow_id {
            url.push_str(&format!("&workflow_id={}", url_encode(w)));
        }
        if let Some(b) = filter.branch {
            url.push_str(&format!("&branch={}", url_encode(b)));
        }
        if let Some(s) = filter.status {
            url.push_str(&format!("&status={}", url_encode(s)));
        }
        let resp = self.client.get(&url).send().await?;
        self.handle_response(resp).await
    }

    /// Retrieves full details of a specific build: `GET /api/v3/builds/{build_id}`.
    pub async fn get_build(&self, build_id: &str) -> Result<Build> {
        let url = format!("{}/api/v3/builds/{}", self.v3_base, build_id);
        let resp = self.client.get(&url).send().await?;
        let wrapper: DataWrapper<Build> = self.handle_response(resp).await?;
        Ok(wrapper.data)
    }

    /// Starts a new build for an application: `POST /api/v3/apps/{app_id}/builds`.
    pub async fn start_build(&self, app_id: &str, req: &StartBuildRequest) -> Result<StartedBuild> {
        let url = format!("{}/api/v3/apps/{}/builds", self.v3_base, app_id);
        let resp = self.client.post(&url).json(req).send().await?;
        let wrapper: DataWrapper<StartedBuild> = self.handle_response(resp).await?;
        Ok(wrapper.data)
    }

    /// Cancels an active or queued build: `POST https://api.codemagic.io/builds/{build_id}/cancel`.
    ///
    /// Returns `Ok(true)` if cancelled, or `Ok(false)` if the build had already finished (HTTP 208).
    pub async fn cancel_build(&self, build_id: &str) -> Result<bool> {
        let url = format!("{}/builds/{}/cancel", self.classic_base, build_id);
        let resp = self.client.post(&url).send().await?;
        let status = resp.status();
        if status.as_u16() == 208 {
            return Ok(false);
        }
        if status.is_success() {
            return Ok(true);
        }
        let text = resp.text().await.unwrap_or_default();
        Err(CodemagicError::ApiError {
            status: status.as_u16(),
            message: text,
        })
    }

    // -------------------------------------------------------------------------
    // Artifacts API
    // -------------------------------------------------------------------------

    /// Generates an expiring public download URL for an artifact: `POST https://api.codemagic.io/artifacts/{secure_path}/public-url`.
    pub async fn create_public_artifact_url(
        &self,
        secure_path: &str,
        expires_at: u64,
    ) -> Result<CreatePublicUrlResponse> {
        let trimmed_path = secure_path.trim_start_matches('/');
        let url = format!(
            "{}/artifacts/{}/public-url",
            self.classic_base, trimmed_path
        );
        let body = CreatePublicUrlRequest { expires_at };
        let resp = self.client.post(&url).json(&body).send().await?;
        self.handle_response(resp).await
    }

    /// Downloads a file from `download_url` to `output_path`, invoking `progress_cb(downloaded_bytes, total_bytes)` as chunks arrive.
    ///
    /// If `download_url` points to a Codemagic API host (`v3_base` or `classic_base`), the initial
    /// request includes `x-auth-token` with automatic redirects disabled. If the server responds
    /// with a `3xx` redirect to an external storage bucket (e.g. AWS S3 or Google Cloud Storage),
    /// the redirect target is fetched via an unauthenticated client so `x-auth-token` is never
    /// leaked or rejected by presigned URL signature checks.
    pub async fn download_file(
        &self,
        download_url: &str,
        output_path: &Path,
        progress_cb: Option<impl Fn(u64, u64)>,
    ) -> Result<()> {
        let is_codemagic_url =
            download_url.starts_with(&self.classic_base) || download_url.starts_with(&self.v3_base);

        let resp = if is_codemagic_url {
            let initial_resp = self.no_redirect_client.get(download_url).send().await?;
            if initial_resp.status().is_redirection()
                && let Some(loc) = initial_resp
                    .headers()
                    .get(LOCATION)
                    .and_then(|v| v.to_str().ok())
            {
                let redirect_url = loc.to_string();
                self.download_client.get(&redirect_url).send().await?
            } else {
                initial_resp
            }
        } else {
            self.download_client.get(download_url).send().await?
        };

        if !resp.status().is_success() {
            return Err(CodemagicError::ApiError {
                status: resp.status().as_u16(),
                message: "Failed to download artifact".to_string(),
            });
        }

        let total_size = resp.content_length().unwrap_or(0);
        let mut file = File::create(output_path).await?;
        let mut stream = resp.bytes_stream();
        let mut downloaded: u64 = 0;

        while let Some(chunk_res) = stream.next().await {
            let chunk = chunk_res?;
            file.write_all(&chunk).await?;
            downloaded += chunk.len() as u64;
            if let Some(ref cb) = progress_cb {
                cb(downloaded, total_size);
            }
        }

        file.flush().await?;
        Ok(())
    }
}

/// Percent-encodes query parameter strings according to RFC 3986 unreserved characters `[a-zA-Z0-9-_.~]`.
pub fn url_encode(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                result.push(b as char);
            }
            _ => {
                result.push_str(&format!("%{:02X}", b));
            }
        }
    }
    result
}
