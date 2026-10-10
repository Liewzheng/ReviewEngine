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

/// Resolve the API base URL the provider client should target.
fn api_base_from_mr_url(mr_url: &str) -> String {
    let (scheme, host) = mr_url_scheme_host(mr_url);
    if host.is_empty() {
        // No host: caller passed something we cannot route. The provider
        // client will fail to construct; the orchestrator records the
        // failure on the FetchedTicket.
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
    let api_base = api_base_from_mr_url(mr_url);
    match ticket.provider {
        super::TicketProvider::GitHub => fetch_github(token, &api_base, ticket).await,
        super::TicketProvider::GitLab => fetch_gitlab(token, &api_base, ticket).await,
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
    fn api_base_from_mr_url_preserves_http() {
        assert_eq!(
            api_base_from_mr_url("http://gitlab.local:8080/g/p/-/merge_requests/1"),
            "http://gitlab.local:8080"
        );
    }

    #[test]
    fn api_base_from_mr_url_returns_invalid_when_no_scheme() {
        assert_eq!(api_base_from_mr_url("garbage.invalid/x/y"), "https://invalid.invalid");
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
