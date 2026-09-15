//! Where the adjudication pass gets the full content of a cited file (RENG-31).
//!
//! The adjudicator judges each finding against the *complete* file at the
//! reviewed revision — the diff patch alone is never a substitute, because a
//! unified diff carries only the changed regions ±3 context lines and
//! adjudicating against it would risk fail-closed drops on code the patch
//! simply does not show. Historically the content came from a local checkout
//! only, which made the whole pass inert on the deployed (server-side) path:
//! webhook/API reviews fetch the diff through the provider API and never
//! clone, so `project_path` is a provider slug (`group/project`) and every
//! candidate passed through unadjudicated.
//!
//! This module is the seam that fixes that. [`FileSource`] abstracts "give me
//! the full text of this repository-relative path", with two implementations:
//!
//! - [`LocalFileSource`] — reads the local checkout (the CLI path; behaviour
//!   unchanged, and still the source used whenever `project_path` is a
//!   directory);
//! - [`ProviderFileSource`] — reads through the provider API at the reviewed
//!   commit SHA, reusing the existing HTTP client and credential plumbing
//!   (GitLab `GET /projects/:id/repository/files/:urlencoded_path/raw?ref=<sha>`,
//!   GitHub `GET /repos/:owner/:repo/contents/:path?ref=<sha>` with
//!   `application/vnd.github.raw+json`), so server-side reviews adjudicate
//!   like local ones.
//!
//! Every failure is a [`FileReadError`], never a silent empty file: the
//! adjudicator keeps each affected group's findings unchanged (fail-open) and
//! says why.

use std::sync::Arc;

use async_trait::async_trait;

use crate::git_provider::FileFetchError;

/// Why the ground-truth content of a file could not be obtained.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileReadError {
    /// The provider reports no such file at the reviewed revision (HTTP 404).
    /// Not a token problem — the file was added by the MR, deleted by it, or
    /// the finding is anchored to a path that never existed.
    NotFound,
    /// The provider rejected the read (HTTP 401/403): the credential lacks
    /// repository read access (GitLab `read_repository`, GitHub contents
    /// read). Reported once per pass, not once per file.
    Unauthorized(String),
    /// Anything else: transport failure, non-UTF-8 content, a path that
    /// escapes the repository root, the fetch timeout, an unreadable local
    /// file.
    Other(String),
}

impl std::fmt::Display for FileReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FileReadError::NotFound => f.write_str("not present at the reviewed revision (404)"),
            FileReadError::Unauthorized(msg) => write!(f, "the token may not read the repository (401/403): {msg}"),
            FileReadError::Other(msg) => f.write_str(msg),
        }
    }
}

/// A source of full file content for the adjudication pass.
#[async_trait]
pub trait FileSource: Send + Sync {
    /// Full text of `path`, relative to the repository root.
    async fn read_file(&self, path: &str) -> Result<String, FileReadError>;

    /// Tag for the adjudication summary log: `local` or `provider-api`.
    fn kind(&self) -> &'static str;

    /// Revision the content is pinned to (a commit SHA) when the source has
    /// one; `None` for a working-tree checkout, which is whatever the
    /// checkout currently holds.
    fn revision(&self) -> Option<&str>;
}

/// Reject paths that would escape the repository root.
///
/// Applied by every source so a finding citing an absolute path or `..` can
/// never make the adjudicator read something outside the repository.
fn validate_relative_path(path: &str) -> Result<(), FileReadError> {
    let rel = std::path::Path::new(path);
    if path.is_empty() || rel.is_absolute() || rel.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return Err(FileReadError::Other("path escapes the project root".to_string()));
    }
    Ok(())
}

/// Full-content reader over a local checkout.
pub struct LocalFileSource {
    root: std::path::PathBuf,
}

impl LocalFileSource {
    pub fn new(root: impl Into<std::path::PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

#[async_trait]
impl FileSource for LocalFileSource {
    async fn read_file(&self, path: &str) -> Result<String, FileReadError> {
        validate_relative_path(path)?;
        let full = self.root.join(path);
        let bytes = std::fs::read(&full)
            .map_err(|_| FileReadError::Other("not readable from the local checkout".to_string()))?;
        String::from_utf8(bytes).map_err(|_| FileReadError::Other("not valid UTF-8".to_string()))
    }

    fn kind(&self) -> &'static str {
        "local"
    }

    fn revision(&self) -> Option<&str> {
        None
    }
}

/// The provider client behind a [`ProviderFileSource`].
enum ProviderClient {
    GitLab(Box<crate::git_provider::gitlab::client::Client>),
    GitHub(Box<crate::git_provider::github::client::Client>),
}

/// Full-content reader over the GitLab/GitHub API at a fixed commit SHA.
pub struct ProviderFileSource {
    client: ProviderClient,
    revision: String,
}

impl ProviderFileSource {
    /// GitLab source over the API base derived from `mr_url`.
    pub fn gitlab(token: &str, mr_url: &str, revision: &str) -> anyhow::Result<Self> {
        let client = crate::git_provider::gitlab::client::Client::new(token, mr_url)?;
        Ok(Self {
            client: ProviderClient::GitLab(Box::new(client)),
            revision: revision.to_string(),
        })
    }

    /// GitHub source over the API base derived from `pr_url`.
    pub fn github(token: &str, pr_url: &str, revision: &str) -> anyhow::Result<Self> {
        let client = crate::git_provider::github::client::Client::new(token, pr_url)?;
        Ok(Self {
            client: ProviderClient::GitHub(Box::new(client)),
            revision: revision.to_string(),
        })
    }

    /// [`github`](Self::github) over an already-constructed client, so a
    /// caller (or a test pointing the client's API base at a mock server)
    /// does not have to re-derive the provider from a URL.
    pub fn github_from_client(
        client: &crate::git_provider::github::client::Client,
        revision: &str,
    ) -> anyhow::Result<Self> {
        if revision.trim().is_empty() {
            anyhow::bail!("the reviewed commit SHA is unknown");
        }
        Ok(Self {
            client: ProviderClient::GitHub(Box::new(client.clone())),
            revision: revision.to_string(),
        })
    }
}

#[async_trait]
impl FileSource for ProviderFileSource {
    async fn read_file(&self, path: &str) -> Result<String, FileReadError> {
        validate_relative_path(path)?;
        let fetched = match &self.client {
            ProviderClient::GitLab(c) => c.fetch_file_raw_checked(path, &self.revision).await,
            ProviderClient::GitHub(c) => c.fetch_file_raw_checked(path, &self.revision).await,
        };
        fetched.map_err(|e: FileFetchError| {
            if e.is_not_found() {
                FileReadError::NotFound
            } else if e.is_unauthorized() {
                FileReadError::Unauthorized(e.message)
            } else {
                FileReadError::Other(e.message)
            }
        })
    }

    fn kind(&self) -> &'static str {
        "provider-api"
    }

    fn revision(&self) -> Option<&str> {
        Some(&self.revision)
    }
}

/// Is this review URL a GitHub PR? Mirrors the provider selection the rest of
/// the codebase uses (`src/lib.rs`, `src/cli/handlers/review.rs`).
fn is_github_url(url: &str) -> bool {
    url.contains(".github.") || url.contains("github.com")
}

/// Build the provider-API file source for a server-side review.
///
/// Fails — never silently degrades — when the inputs the fetch needs are
/// missing: the review URL must be a usable MR/PR URL, the credential must be
/// non-empty, and the reviewed SHA must be known (a review that does not know
/// which revision it reviewed cannot ask for that revision's files, and the
/// adjudicator will fail open with a warning).
pub fn provider_source(mr_url: &str, token: &str, revision: &str) -> anyhow::Result<Arc<dyn FileSource>> {
    if mr_url.trim().is_empty() {
        anyhow::bail!("no review URL");
    }
    if token.trim().is_empty() {
        anyhow::bail!("no provider token");
    }
    if revision.trim().is_empty() {
        anyhow::bail!("the reviewed commit SHA is unknown");
    }
    let source: Arc<dyn FileSource> = if is_github_url(mr_url) {
        Arc::new(ProviderFileSource::github(token, mr_url, revision)?)
    } else {
        Arc::new(ProviderFileSource::gitlab(token, mr_url, revision)?)
    };
    Ok(source)
}

/// [`provider_source`] for call sites that only have the warning to add: logs
/// why server-side adjudication will run without provider file access and
/// returns `None`, so the pass fails open loudly instead of silently.
pub fn provider_source_or_warn(mr_url: &str, token: &str, revision: &str) -> Option<Arc<dyn FileSource>> {
    match provider_source(mr_url, token, revision) {
        Ok(source) => Some(source),
        Err(e) => {
            warn_without_file_source(&e);
            None
        }
    }
}

/// Provider source built from an already-constructed GitLab client (its API
/// base and credential are bound to the reviewed MR) — the shape the API
/// review path has in hand. `None` with the same warning as
/// [`provider_source_or_warn`] when the reviewed SHA is unknown.
pub fn provider_source_from_client_or_warn(
    client: &crate::git_provider::gitlab::client::Client,
    revision: &str,
) -> Option<Arc<dyn FileSource>> {
    if revision.trim().is_empty() {
        warn_without_file_source(&anyhow::anyhow!("the reviewed commit SHA is unknown"));
        return None;
    }
    Some(Arc::new(ProviderFileSource {
        client: ProviderClient::GitLab(Box::new(client.clone())),
        revision: revision.to_string(),
    }))
}

/// The one warning every caller emits when it cannot provide provider file
/// access: the consequence (unadjudicated findings) must be as visible as the
/// cause.
fn warn_without_file_source(e: &anyhow::Error) {
    tracing::warn!(
        "Adjudication: no provider file source for this review ({e}) — full-file ground truth will be \
         unavailable, so findings at or above the adjudication threshold will pass through \
         unadjudicated (fail-open)"
    );
}

/// The ground-truth source for a review: the local checkout when
/// `project_path` is a directory (unchanged CLI behaviour), else the
/// provider-API source the caller plumbed in, else `None` — the pass then
/// fails open exactly as it always has.
pub fn resolve_ground_truth(project_path: &str, remote: Option<Arc<dyn FileSource>>) -> Option<Arc<dyn FileSource>> {
    if std::path::Path::new(project_path).is_dir() {
        return Some(Arc::new(LocalFileSource::new(project_path)));
    }
    remote
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn validate_rejects_escaping_paths() {
        assert!(validate_relative_path("src/main.rs").is_ok());
        assert!(validate_relative_path("").is_err());
        assert!(validate_relative_path("/etc/passwd").is_err());
        assert!(validate_relative_path("../secret").is_err());
        assert!(validate_relative_path("src/../../secret").is_err());
    }

    #[tokio::test]
    async fn local_source_reads_and_reports_failures() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn main() {}").unwrap();
        let source = LocalFileSource::new(dir.path());

        assert_eq!(source.kind(), "local");
        assert_eq!(source.revision(), None);
        assert_eq!(source.read_file("a.rs").await.unwrap(), "fn main() {}");
        assert!(matches!(
            source.read_file("missing.rs").await,
            Err(FileReadError::Other(_))
        ));
        assert!(matches!(
            source.read_file("../a.rs").await,
            Err(FileReadError::Other(_))
        ));
    }

    #[tokio::test]
    async fn local_source_rejects_non_utf8() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("bin.rs"), [0xff, 0xfe, 0x00]).unwrap();
        let source = LocalFileSource::new(dir.path());
        assert!(matches!(source.read_file("bin.rs").await, Err(FileReadError::Other(_))));
    }

    /// The GitLab endpoint the adjudicator now uses on the server path, driven
    /// through the real client against a mock API.
    #[tokio::test]
    async fn provider_source_fetches_gitlab_file_at_the_reviewed_sha() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/api/v4/projects/group%2Fproject/repository/files/src%2Fmain.rs/raw",
            ))
            .and(query_param("ref", "sha123"))
            .respond_with(ResponseTemplate::new(200).set_body_string("fn main() {}\n"))
            .expect(1)
            .mount(&server)
            .await;

        let url = format!("{}/group/project/-/merge_requests/7", server.uri());
        let source = ProviderFileSource::gitlab("token", &url, "sha123").unwrap();
        assert_eq!(source.kind(), "provider-api");
        assert_eq!(source.revision(), Some("sha123"));
        assert_eq!(source.read_file("src/main.rs").await.unwrap(), "fn main() {}\n");
    }

    /// The GitHub arm of the same seam: contents API at the reviewed SHA, and
    /// the status mapping that decides fail-open behaviour.
    #[tokio::test]
    async fn provider_source_reads_github_files_and_maps_statuses() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/owner/repo/contents/src/main.rs"))
            .and(query_param("ref", "sha123"))
            .and(wiremock::matchers::header("Accept", "application/vnd.github.raw+json"))
            .respond_with(ResponseTemplate::new(200).set_body_string("fn main() {}\n"))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/owner/repo/contents/src/denied.rs"))
            .respond_with(ResponseTemplate::new(403).set_body_string("{\"message\":\"Forbidden\"}"))
            .mount(&server)
            .await;

        let client = crate::git_provider::github::client::Client::new_test(
            "token",
            "https://github.com/owner/repo/pull/3",
            &server.uri(),
        )
        .unwrap();
        let source = ProviderFileSource::github_from_client(&client, "sha123").unwrap();
        assert_eq!(source.kind(), "provider-api");
        assert_eq!(source.read_file("src/main.rs").await.unwrap(), "fn main() {}\n");
        assert!(matches!(
            source.read_file("src/denied.rs").await,
            Err(FileReadError::Unauthorized(_))
        ));
    }

    #[tokio::test]
    async fn provider_source_maps_404_to_not_found() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/api/v4/projects/group%2Fproject/repository/files/src%2Fgone.rs/raw",
            ))
            .respond_with(ResponseTemplate::new(404).set_body_string("{\"message\":\"404 File Not Found\"}"))
            .mount(&server)
            .await;

        let url = format!("{}/group/project/-/merge_requests/7", server.uri());
        let source = ProviderFileSource::gitlab("token", &url, "sha123").unwrap();
        assert_eq!(
            source.read_file("src/gone.rs").await.unwrap_err(),
            FileReadError::NotFound
        );
    }

    #[tokio::test]
    async fn provider_source_maps_401_and_403_to_unauthorized() {
        for status in [401u16, 403] {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path(
                    "/api/v4/projects/group%2Fproject/repository/files/src%2Fmain.rs/raw",
                ))
                .respond_with(ResponseTemplate::new(status).set_body_string("{\"message\":\"denied\"}"))
                .mount(&server)
                .await;

            let url = format!("{}/group/project/-/merge_requests/7", server.uri());
            let source = ProviderFileSource::gitlab("token", &url, "sha123").unwrap();
            match source.read_file("src/main.rs").await {
                Err(FileReadError::Unauthorized(msg)) => assert!(msg.contains(&status.to_string())),
                other => panic!("expected Unauthorized for {status}, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn provider_source_maps_transport_failure_to_other() {
        // Port 1 is not serving HTTP; the failure carries no status.
        let source =
            ProviderFileSource::gitlab("token", "http://127.0.0.1:1/group/project/-/merge_requests/7", "sha123")
                .unwrap();
        assert!(matches!(
            source.read_file("src/main.rs").await,
            Err(FileReadError::Other(_))
        ));
    }

    #[test]
    fn provider_source_requires_url_token_and_revision() {
        let err = provider_source("", "token", "sha").err().unwrap().to_string();
        assert!(err.contains("review URL"), "{err}");
        let err = provider_source("https://gitlab.example.com/g/p/-/merge_requests/1", "  ", "sha")
            .err()
            .unwrap()
            .to_string();
        assert!(err.contains("token"), "{err}");
        let err = provider_source("https://gitlab.example.com/g/p/-/merge_requests/1", "token", "")
            .err()
            .unwrap()
            .to_string();
        assert!(err.contains("SHA"), "{err}");
    }

    #[test]
    fn provider_source_selects_the_provider_by_url() {
        let gl = provider_source("https://gitlab.example.com/g/p/-/merge_requests/1", "t", "sha").unwrap();
        assert_eq!(gl.kind(), "provider-api");
        let gh = provider_source("https://github.com/o/r/pull/3", "t", "sha").unwrap();
        assert_eq!(gh.kind(), "provider-api");
    }

    #[test]
    fn resolve_ground_truth_prefers_the_local_checkout() {
        let dir = tempfile::tempdir().unwrap();
        let local = resolve_ground_truth(dir.path().to_str().unwrap(), None).unwrap();
        assert_eq!(local.kind(), "local");

        // A slug is not a directory: with no remote source plumbed in, the
        // pass has no ground truth at all and must fail open.
        let slug = dir.path().join("group/project");
        assert!(resolve_ground_truth(slug.to_str().unwrap(), None).is_none());

        let remote = provider_source("https://gitlab.example.com/g/p/-/merge_requests/1", "t", "sha").unwrap();
        let resolved = resolve_ground_truth(slug.to_str().unwrap(), Some(remote.clone())).unwrap();
        assert_eq!(resolved.kind(), "provider-api");
        // The checkout still wins when it exists — local behaviour unchanged.
        let resolved = resolve_ground_truth(dir.path().to_str().unwrap(), Some(remote)).unwrap();
        assert_eq!(resolved.kind(), "local");
    }
}
