//! GitHub Releases API client for the self-update flow.
//!
//! Deliberately separate from `git_provider::github::client`, which is coupled
//! to PR URLs. This client only talks to the `releases/latest` endpoint and
//! carries a `review-engine/<current-version>` user agent.

use std::time::Duration;

use reqwest::header::{HeaderValue, ACCEPT};
use reqwest::Client as HttpClient;
use serde::Deserialize;
use tracing::debug;

use super::error::{Result, UpgradeError};
use super::platform::AssetSpec;

const OWNER: &str = "Liewzheng";
const REPO: &str = "ReviewEngine";
const API_BASE: &str = "https://api.github.com";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// One downloadable file attached to a GitHub release.
#[derive(Debug, Clone, Deserialize)]
pub struct ReleaseAsset {
    pub name: String,
    #[serde(rename = "browser_download_url")]
    pub download_url: String,
    /// Byte size reported by the GitHub API.
    pub size: u64,
}

/// A GitHub release as returned by `GET /repos/{owner}/{repo}/releases/latest`.
///
/// Unknown fields are ignored by serde; only what the upgrade flow needs is
/// modelled here.
#[derive(Debug, Clone, Deserialize)]
pub struct Release {
    #[serde(rename = "tag_name")]
    pub tag_name: String,
    #[serde(rename = "html_url")]
    pub html_url: String,
    #[serde(rename = "published_at")]
    pub published_at: String,
    pub assets: Vec<ReleaseAsset>,
}

/// Client for the `releases/latest` endpoint of the review-engine repo.
#[derive(Debug, Clone)]
pub struct GitHubReleaseClient {
    http: HttpClient,
    base_url: String,
    owner: String,
    repo: String,
}

impl GitHubReleaseClient {
    /// Build a client for the canonical repo. `current_version` is embedded in
    /// the `User-Agent` header so GitHub API rate limiting treats us as a
    /// first-class client rather than a bare scraper.
    pub fn new(current_version: &str) -> Result<Self> {
        Self::with_base_url(current_version, API_BASE)
    }

    /// Build a client pointing at a custom API base (a self-hosted mirror, a
    /// staging endpoint, or a wiremock in tests). `new()` is equivalent to
    /// `with_base_url(version, "https://api.github.com")`.
    pub fn with_base_url(current_version: &str, base_url: &str) -> Result<Self> {
        Self::new_at(current_version, base_url, OWNER, REPO)
    }

    /// Test seam: point at a custom API base (e.g. a wiremock server).
    #[cfg(test)]
    pub fn new_for_test(current_version: &str, base_url: &str) -> Result<Self> {
        Self::with_base_url(current_version, base_url)
    }

    fn new_at(current_version: &str, base_url: &str, owner: &str, repo: &str) -> Result<Self> {
        let user_agent = format!("review-engine/{current_version}");
        // `current_version` normally comes from `CARGO_PKG_VERSION`, but a
        // hostile caller could pass anything; fall back to a bare UA rather
        // than panic on a header-value error.
        let ua_value = match HeaderValue::from_str(&user_agent) {
            Ok(v) => v,
            Err(_) => HeaderValue::from_static("review-engine"),
        };
        let http = HttpClient::builder()
            .timeout(REQUEST_TIMEOUT)
            .user_agent(ua_value)
            .build()
            .map_err(UpgradeError::from)?;
        Ok(Self {
            http,
            base_url: base_url.to_string(),
            owner: owner.to_string(),
            repo: repo.to_string(),
        })
    }

    /// The underlying HTTP client (used for asset downloads, which need a
    /// longer timeout than metadata requests).
    pub fn http_client(&self) -> &HttpClient {
        &self.http
    }

    /// `GET /repos/{owner}/{repo}/releases/latest` — the raw latest release.
    pub async fn latest_release(&self) -> Result<Release> {
        let url = format!("{}/repos/{}/{}/releases/latest", self.base_url, self.owner, self.repo);
        let resp = self
            .http
            .get(&url)
            .header(ACCEPT, "application/vnd.github+json")
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(UpgradeError::Api { status, body });
        }
        resp.json().await.map_err(UpgradeError::from)
    }

    /// The latest release whose tag matches `^v\d+\.\d+\.\d+$`.
    ///
    /// GitHub's `latest` endpoint already excludes prereleases/drafts, but a
    /// release can still be tagged oddly (`v0.9.0-rc1`, `stable`, ...). This
    /// returns `Ok(None)` when the latest release is not a stable tag.
    pub async fn latest_stable_release(&self) -> Result<Option<Release>> {
        let release = self.latest_release().await?;
        if super::version::is_stable_release_tag(&release.tag_name) {
            Ok(Some(release))
        } else {
            debug!(tag = %release.tag_name, "latest GitHub release is not a stable vX.Y.Z tag");
            Ok(None)
        }
    }
}

/// Find the asset matching `spec` (e.g. `review-engine-aarch64-apple-darwin.tar.gz`).
pub fn find_asset<'a>(release: &'a Release, spec: &AssetSpec) -> Option<&'a ReleaseAsset> {
    let name = spec.asset_name("review-engine");
    release.assets.iter().find(|a| a.name == name)
}

/// Find the `.sha256` sidecar for an asset name (e.g. `<asset>.sha256`).
pub fn find_checksum_asset<'a>(release: &'a Release, asset_name: &str) -> Option<&'a ReleaseAsset> {
    let name = format!("{asset_name}.sha256");
    release.assets.iter().find(|a| a.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn release_json(tag: &str) -> serde_json::Value {
        json!({
            "tag_name": tag,
            "html_url": format!("https://github.com/Liewzheng/ReviewEngine/releases/tag/{tag}"),
            "published_at": "2024-06-01T00:00:00Z",
            "assets": [
                {
                    "name": "review-engine-aarch64-apple-darwin.tar.gz",
                    "browser_download_url": "https://example.com/aarch64-apple-darwin.tar.gz",
                    "size": 100
                },
                {
                    "name": "review-engine-x86_64-unknown-linux-gnu.tar.gz",
                    "browser_download_url": "https://example.com/x86_64-linux.tar.gz",
                    "size": 200
                },
                {
                    "name": "review-engine-x86_64-unknown-linux-gnu.tar.gz.sha256",
                    "browser_download_url": "https://example.com/x86_64-linux.tar.gz.sha256",
                    "size": 72
                }
            ]
        })
    }

    #[tokio::test]
    async fn parses_latest_release_and_sends_ua() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/Liewzheng/ReviewEngine/releases/latest"))
            .and(header("User-Agent", "review-engine/0.8.2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(release_json("v0.9.0")))
            .mount(&server)
            .await;

        let client = GitHubReleaseClient::new_for_test("0.8.2", &server.uri()).unwrap();
        let release = client.latest_release().await.unwrap();
        assert_eq!(release.tag_name, "v0.9.0");
        assert_eq!(
            release.html_url,
            "https://github.com/Liewzheng/ReviewEngine/releases/tag/v0.9.0"
        );
        assert_eq!(release.published_at, "2024-06-01T00:00:00Z");
        assert_eq!(release.assets.len(), 3);
        assert_eq!(release.assets[0].name, "review-engine-aarch64-apple-darwin.tar.gz");
        assert_eq!(
            release.assets[0].download_url,
            "https://example.com/aarch64-apple-darwin.tar.gz"
        );
        assert_eq!(release.assets[0].size, 100);
    }

    #[tokio::test]
    async fn latest_stable_accepts_stable_tag() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/Liewzheng/ReviewEngine/releases/latest"))
            .respond_with(ResponseTemplate::new(200).set_body_json(release_json("v0.9.0")))
            .mount(&server)
            .await;

        let client = GitHubReleaseClient::new_for_test("0.8.2", &server.uri()).unwrap();
        let release = client.latest_stable_release().await.unwrap().expect("v0.9.0 is stable");
        assert_eq!(release.tag_name, "v0.9.0");
    }

    #[tokio::test]
    async fn latest_stable_skips_non_semver_tag() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/Liewzheng/ReviewEngine/releases/latest"))
            .respond_with(ResponseTemplate::new(200).set_body_json(release_json("v0.9.0-rc1")))
            .mount(&server)
            .await;

        let client = GitHubReleaseClient::new_for_test("0.8.2", &server.uri()).unwrap();
        assert!(client.latest_stable_release().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn api_error_surfaces_status() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/Liewzheng/ReviewEngine/releases/latest"))
            .respond_with(ResponseTemplate::new(403).set_body_json(json!({"message": "rate limit exceeded"})))
            .mount(&server)
            .await;

        let client = GitHubReleaseClient::new_for_test("0.8.2", &server.uri()).unwrap();
        let err = client.latest_release().await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("403"), "error should mention 403, got: {msg}");
    }

    #[tokio::test]
    async fn malformed_json_is_invalid_data() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/Liewzheng/ReviewEngine/releases/latest"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json at all"))
            .mount(&server)
            .await;

        let client = GitHubReleaseClient::new_for_test("0.8.2", &server.uri()).unwrap();
        assert!(client.latest_release().await.is_err());
    }

    #[test]
    fn finds_platform_asset_by_triple() {
        let release: Release = serde_json::from_value(release_json("v0.9.0")).expect("valid release json");
        let spec = super::super::platform::asset_spec_for("linux", "x86_64").unwrap();
        let asset = find_asset(&release, &spec).expect("linux x86_64 asset exists");
        assert_eq!(asset.name, "review-engine-x86_64-unknown-linux-gnu.tar.gz");

        let checksum = find_checksum_asset(&release, &asset.name).expect("checksum asset exists");
        assert_eq!(checksum.name, "review-engine-x86_64-unknown-linux-gnu.tar.gz.sha256");

        // The fixture has no Windows asset — must not match the linux one.
        let absent = super::super::platform::asset_spec_for("windows", "x86_64").unwrap();
        assert!(find_asset(&release, &absent).is_none());
    }
}
