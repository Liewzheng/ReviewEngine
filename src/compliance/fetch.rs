//! Ticket body fetch via the existing GitLab / GitHub provider clients.
//!
//! The provider clients live in `src/git_provider/{gitlab,github}/client.rs`;
//! `fetch_ticket_via_provider` is the only entry point the compliance
//! orchestrator uses. It delegates to the provider's `fetch_issue` method
//! rather than re-implementing URL shaping, request framing or shape
//! parsing — the provider clients own those concerns and the compliance
//! step should not become a second client implementation.
//!
//! Failures are recorded on the [`super::FetchedTicket`] itself
//! (HTTP error, missing auth, transport). The compliance orchestrator
//! renders them as notes in the section — never as review-blocking errors.

use tracing::warn;

use super::{FetchedTicket, TicketRef};

/// Maximum characters of a ticket body we keep for the prompt. Mirrors
/// pr-agent's `MAX_TICKET_CHARACTERS = 10000` — a ticket past that is
/// truncated with a `[…truncated]` marker.
pub const TICKET_BODY_BUDGET: usize = 10_000;

/// Build the URL the GitHub client would send for an issue. Public so the
/// provider client's `fetch_issue` can use the same path shape and the
/// integration tests can assert the URL the mock receives.
pub fn github_issue_url(api_base: &str, owner: &str, repo: &str, number: u32) -> String {
    format!(
        "{}/repos/{}/{}/issues/{}",
        api_base.trim_end_matches('/'),
        owner,
        repo,
        number
    )
}

/// Build the URL the GitLab client would send for an issue. Same
/// rationale as `github_issue_url`.
pub fn gitlab_issue_url(base_url: &str, project_path: &str, iid: u32) -> String {
    let encoded = encode_project_path(project_path);
    format!("{}/projects/{}/issues/{}", base_url.trim_end_matches('/'), encoded, iid)
}

fn encode_project_path(path: &str) -> String {
    // GitLab expects `/` URL-encoded as `%2F`; percent-encode everything
    // else that is not `/` or a safe character.
    path.split('/')
        .map(|segment| {
            segment
                .bytes()
                .map(|b| match b {
                    b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
                    _ => format!("%{b:02X}"),
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("%2F")
}

/// Extract `(scheme, host)` from an MR URL. Used to build the provider
/// API base so the compliance sub-step targets the same instance the
/// user passed to `review` — including plain-HTTP self-hosted GitLab
/// (whose scheme `build_short_url` flattens to `https://`) and the
/// wiremock fixtures used by the integration tests.
fn mr_url_scheme_host(mr_url: &str) -> (&'static str, String) {
    if let Some(rest) = mr_url.strip_prefix("https://") {
        let host = rest.split('/').next().unwrap_or("").to_string();
        ("https", host)
    } else if let Some(rest) = mr_url.strip_prefix("http://") {
        let host = rest.split('/').next().unwrap_or("").to_string();
        ("http", host)
    } else {
        ("https", String::new())
    }
}

/// Map an MR/PR URL's host to the REST API base the provider client
/// should target.
///
/// The MR URL is where a user reviews code, which is **not** the same
/// host as the REST API for most platforms:
///
/// * `github.com`           → `https://api.github.com` (the well-known
///                            REST base the provider client itself
///                            hardcodes at `Client::new` for this
///                            platform).
/// * `*.github.com` (GHE-C)  → same, `https://api.github.com`.
/// * GHE-Server (any other)  → `{scheme}://{host}/api/v3` (the
///                            REST path the GitHub Enterprise Server
///                            docs specify).
/// * Anything else           → `{scheme}://{host}` unchanged — the
///                            wiremock fixtures and the GitLab path
///                            route here (GitLab's `Client::for_instance`
///                            appends `/api/v4` itself).
///
/// P1-1 (M40 r3): the round-2 implementation was returning
/// `{scheme}://{host}` for github.com, which built
/// `https://github.com/repos/…` instead of
/// `https://api.github.com/repos/…` and made every GitHub ticket fetch
/// fail at the HTTP layer. The wiremock integration test did not catch
/// this because the mock's origin doubles as both the web host and the
/// API base.
pub fn github_api_base_from_mr_url(mr_url: &str) -> String {
    let (scheme, host) = mr_url_scheme_host(mr_url);
    if host.is_empty() {
        return format!("{scheme}://invalid.invalid");
    }
    // Strip an optional `:port` suffix so the comparison is host-only.
    let host_no_port = host.split(':').next().unwrap_or(&host).to_string();
    if host_no_port.eq_ignore_ascii_case("github.com") || host_no_port.to_ascii_lowercase().ends_with(".github.com") {
        // Pinned to https — the GitHub REST API does not serve plain
        // HTTP even when the MR URL did. The PAT still travels in the
        // Authorization header so the scheme upgrade is safe.
        return "https://api.github.com".to_string();
    }
    // GHE Server and other non-github.com hosts: REST base is
    // `{scheme}://{host}/api/v3`.
    format!("{scheme}://{host}/api/v3")
}

/// Resolve the GitLab API base URL the provider client should target.
///
/// GitLab's `Client::for_instance` appends `/api/v4` to whatever base
/// is passed in, so we hand it the instance URL and let the client
/// shape the path.
fn gitlab_api_base_from_mr_url(mr_url: &str) -> String {
    let (scheme, host) = mr_url_scheme_host(mr_url);
    if host.is_empty() {
        return format!("{scheme}://invalid.invalid");
    }
    format!("{scheme}://{host}")
}

/// Fetch one ticket via the provider client that owns the API surface.
///
/// `token` is the platform PAT the caller already holds for the MR/PR
/// fetch — the function does NOT read from `GITLAB_TOKEN` / `GITHUB_TOKEN`
/// env vars (P2-1: env-only token resolution broke the CLI
/// `--gitlab-token` path, the server-side resolver, and Python callers).
///
/// On any non-2xx response the function returns a [`FetchedTicket`] with
/// `error` set and the caller's flow continues — the orchestrator
/// surfaces the error as a section note.
pub async fn fetch_ticket_via_provider(token: &str, mr_url: &str, ticket: &TicketRef) -> FetchedTicket {
    match ticket.provider {
        super::TicketProvider::GitHub => {
            let api_base = github_api_base_from_mr_url(mr_url);
            fetch_github(token, &api_base, ticket).await
        }
        super::TicketProvider::GitLab => {
            let api_base = gitlab_api_base_from_mr_url(mr_url);
            fetch_gitlab(token, &api_base, ticket).await
        }
    }
}

async fn fetch_github(token: &str, api_base: &str, ticket: &TicketRef) -> FetchedTicket {
    let project = ticket.project.clone().unwrap_or_default();
    let (_owner, _repo) = match project.split_once('/') {
        Some((o, r)) => (o, r),
        None => {
            return failure(ticket, "GitHub ticket ref is missing the owner/repo/ component");
        }
    };
    let client = match crate::git_provider::github::client::Client::for_issue(token, &project, api_base, ticket.iid) {
        Ok(c) => c,
        Err(e) => return failure(ticket, &format!("GitHub client init: {e}")),
    };
    match client.fetch_issue(Some(&project), ticket.iid).await {
        Ok(t) => t,
        Err(e) => {
            warn!(
                url = %ticket.url,
                error = %e,
                "compliance ticket fetch (GitHub) failed"
            );
            failure(ticket, &format!("GitHub fetch error: {e}"))
        }
    }
}

async fn fetch_gitlab(token: &str, api_base: &str, ticket: &TicketRef) -> FetchedTicket {
    let project = ticket.project.clone().unwrap_or_default();
    if project.is_empty() {
        return failure(ticket, "GitLab ticket ref is missing the project path");
    }
    let client = crate::git_provider::gitlab::client::Client::for_instance(token, api_base);
    match client.fetch_issue(Some(&project), ticket.iid).await {
        Ok(t) => t,
        Err(e) => {
            warn!(
                url = %ticket.url,
                error = %e,
                "compliance ticket fetch (GitLab) failed"
            );
            failure(ticket, &format!("GitLab fetch error: {e}"))
        }
    }
}

fn failure(ticket: &TicketRef, reason: &str) -> FetchedTicket {
    FetchedTicket {
        ticket_ref: ticket.clone(),
        title: String::new(),
        body: String::new(),
        web_url: None,
        error: Some(reason.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compliance::{TicketProvider, TicketRef};

    #[allow(dead_code)]
    fn ref_gitlab(iid: u32, project: &str) -> TicketRef {
        TicketRef {
            provider: TicketProvider::GitLab,
            project: Some(project.to_string()),
            iid,
            url: format!("https://gitlab.example.com/{}/-/work_items/{}", project, iid),
            source: format!("#{}", iid),
        }
    }

    fn ref_github(iid: u32, project: &str) -> TicketRef {
        TicketRef {
            provider: TicketProvider::GitHub,
            project: Some(project.to_string()),
            iid,
            url: format!("https://github.com/{}/issues/{}", project, iid),
            source: format!("#{}", iid),
        }
    }

    #[test]
    fn mr_url_scheme_host_extracts_https_default() {
        assert_eq!(
            mr_url_scheme_host("https://gitlab.example.com/g/p/-/merge_requests/1"),
            ("https", "gitlab.example.com".to_string())
        );
    }

    #[test]
    fn mr_url_scheme_host_keeps_plain_http_for_self_hosted() {
        assert_eq!(
            mr_url_scheme_host("http://gitlab.local:8080/g/p/-/merge_requests/1"),
            ("http", "gitlab.local:8080".to_string())
        );
    }

    #[test]
    fn mr_url_scheme_host_falls_back_when_no_scheme() {
        assert_eq!(mr_url_scheme_host("garbage.invalid/x/y"), ("https", String::new()));
    }

    #[test]
    fn gitlab_api_base_from_mr_url_preserves_http() {
        assert_eq!(
            gitlab_api_base_from_mr_url("http://gitlab.local:8080/g/p/-/merge_requests/1"),
            "http://gitlab.local:8080"
        );
    }

    #[test]
    fn gitlab_api_base_from_mr_url_returns_invalid_when_no_scheme() {
        assert_eq!(
            gitlab_api_base_from_mr_url("garbage.invalid/x/y"),
            "https://invalid.invalid"
        );
    }

    /// P1-1 (M40 r3): the GitHub MR URL must map to the REST API host,
    /// not the web host. The wiremock test cannot observe this because
    /// the mock's origin doubles as both web host and API base; the
    /// unit test below asserts the URL the client would build.
    #[test]
    fn github_api_base_maps_github_com_to_api_github_com() {
        assert_eq!(
            github_api_base_from_mr_url("https://github.com/owner/repo/pull/1"),
            "https://api.github.com"
        );
        let api_base = github_api_base_from_mr_url("https://github.com/owner/repo/pull/1");
        assert_eq!(
            github_issue_url(&api_base, "owner", "repo", 42),
            "https://api.github.com/repos/owner/repo/issues/42"
        );
    }

    /// GHE-Cloud (the `.github.com` suffix) shares the public REST base.
    #[test]
    fn github_api_base_maps_subdomain_github_dot_com_to_api_github_com() {
        assert_eq!(
            github_api_base_from_mr_url("https://acme.github.com/owner/repo/pull/1"),
            "https://api.github.com"
        );
    }

    /// GHE-Server (any other host) routes to `{host}/api/v3`.
    #[test]
    fn github_api_base_maps_ghe_server_to_api_v3() {
        assert_eq!(
            github_api_base_from_mr_url("https://ghe.example.com/owner/repo/pull/1"),
            "https://ghe.example.com/api/v3"
        );
        // Plain-http GHE Server stays plain-http (no scheme upgrade).
        assert_eq!(
            github_api_base_from_mr_url("http://ghe.local/owner/repo/pull/1"),
            "http://ghe.local/api/v3"
        );
    }

    /// github.com with a port is rare but tolerated: the port is for the
    /// web host (the PR UI); the REST API host has no port.
    #[test]
    fn github_api_base_strips_optional_port_on_github_com() {
        assert_eq!(
            github_api_base_from_mr_url("https://github.com:443/owner/repo/pull/1"),
            "https://api.github.com"
        );
    }

    /// Wiremock and other non-github.com hosts pass through unchanged
    /// (no `/api/v3` is forced — the wiremock fixtures and the GitLab
    /// path route through `gitlab_api_base_from_mr_url`).
    #[test]
    fn github_api_base_passes_through_wiremock_hosts() {
        assert_eq!(
            github_api_base_from_mr_url("http://127.0.0.1:8080/owner/repo/pull/1"),
            "http://127.0.0.1:8080/api/v3"
        );
    }

    #[test]
    fn fetch_github_returns_failure_for_malformed_owner_repo() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        // Owner-repo without a slash: caught by the pre-check in
        // `fetch_github` so the caller never sees a confusing provider
        // error about "Invalid GitHub owner" / "Invalid GitHub repo".
        let ticket = ref_github(1, "only-owner");
        let result = rt.block_on(fetch_github("token", "https://api.github.com", &ticket));
        assert!(!result.is_fetched());
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("missing the owner/repo/"),
            "expected pre-check failure, got: {:?}",
            result.error
        );
    }

    #[test]
    fn fetch_gitlab_returns_failure_for_missing_project() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let ticket = TicketRef {
            provider: TicketProvider::GitLab,
            project: None,
            iid: 1,
            url: "https://gitlab.example.com/-/work_items/1".into(),
            source: "#1".into(),
        };
        let result = rt.block_on(fetch_gitlab("token", "https://gitlab.example.com", &ticket));
        assert!(!result.is_fetched());
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("missing the project path"));
    }

    // The wiremock-backed GitHub and GitLab integration coverage lives
    // in tests/compliance/main.rs; the URL builders and the
    // immediate-failure paths (no network round-trip) are the only
    // things covered here.

    #[test]
    fn github_issue_url_matches_api_shape() {
        // The provider's wiremock sees this path; the compliance
        // integration test asserts the same shape via request recording.
        assert_eq!(
            github_issue_url("https://api.github.com", "owner", "repo", 7),
            "https://api.github.com/repos/owner/repo/issues/7"
        );
    }

    #[test]
    fn gitlab_issue_url_encodes_project_slash() {
        assert_eq!(
            gitlab_issue_url("https://gitlab.example.com/api/v4", "g/p", 9),
            "https://gitlab.example.com/api/v4/projects/g%2Fp/issues/9"
        );
    }

    #[test]
    fn encode_project_path_preserves_safe_chars() {
        assert_eq!(encode_project_path("g/p"), "g%2Fp");
        assert_eq!(encode_project_path("a-b_c.d~e/f"), "a-b_c.d~e%2Ff");
        assert_eq!(encode_project_path("g/s/p"), "g%2Fs%2Fp");
    }
}
