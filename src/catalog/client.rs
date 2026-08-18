//! HTTP client for the models.dev catalog endpoint.
//!
//! Follows the `upgrade::github_release` template: a versioned
//! `review-engine/<ver>` User-Agent, a 15-second request timeout, typed serde
//! structs, status-checked errors, and a `with_base_url` seam so tests can
//! point at a wiremock server.

use std::time::Duration;

use reqwest::header::HeaderValue;
use reqwest::Client as HttpClient;

use super::{Catalog, CatalogError, Result, API_BASE_ENV, DEFAULT_API_BASE};

/// Catalog fetches are on interactive paths (init prompt, UI page load), so
/// keep the timeout tighter than the 30s upgrade flow.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Client for `GET {base}/api.json` on models.dev (or a mirror).
#[derive(Debug, Clone)]
pub struct CatalogClient {
    http: HttpClient,
    base_url: String,
}

impl CatalogClient {
    /// Build a client for the canonical models.dev endpoint.
    pub fn new() -> Result<Self> {
        Self::with_base_url(DEFAULT_API_BASE)
    }

    /// Build a client honoring the `REVIEW_MODELS_DEV_API_BASE` env override,
    /// falling back to the canonical endpoint. Shared by the server handlers
    /// and the CLI init flow so both respect the same seam.
    pub fn from_env() -> Result<Self> {
        let base = std::env::var(API_BASE_ENV).unwrap_or_else(|_| DEFAULT_API_BASE.to_string());
        Self::with_base_url(&base)
    }

    /// Build a client pointing at a custom API base (a mirror, or a wiremock
    /// server in tests). `new()` is equivalent to
    /// `with_base_url("https://models.dev")`.
    pub fn with_base_url(base_url: &str) -> Result<Self> {
        let user_agent = format!("review-engine/{}", env!("CARGO_PKG_VERSION"));
        // The version normally comes from CARGO_PKG_VERSION; fall back to a
        // bare UA rather than panic on a header-value error.
        let ua_value = match HeaderValue::from_str(&user_agent) {
            Ok(v) => v,
            Err(_) => HeaderValue::from_static("review-engine"),
        };
        let http = HttpClient::builder()
            .timeout(REQUEST_TIMEOUT)
            .user_agent(ua_value)
            .build()
            .map_err(CatalogError::from)?;
        Ok(Self {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
        })
    }

    /// `GET {base}/api.json` — the full provider catalog.
    pub async fn fetch_catalog(&self) -> Result<Catalog> {
        let url = format!("{}/api.json", self.base_url);
        let resp = self.http.get(&url).send().await?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(CatalogError::Api { status, body });
        }
        resp.json().await.map_err(CatalogError::from)
    }
}
