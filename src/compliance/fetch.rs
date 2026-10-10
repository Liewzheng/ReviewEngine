//! Ticket body fetch via the existing GitLab / GitHub provider clients.
//!
//! The provider clients live in `src/git_provider/{gitlab,github}/client.rs`
//! and each gets a [`crate::compliance::fetch::fetch_ticket_via_provider`]
//! entry point. The compliance sub-step asks the provider to fetch the
//! ticket's title + description; the response is normalised into a
//! [`super::FetchedTicket`] and returned to the caller.
//!
//! Failures are recorded on the [`super::FetchedTicket`] itself
//! (HTTP error, missing auth, transport). The compliance orchestrator
//! renders them as notes in the section — never as review-blocking errors.

use anyhow::Result;
use reqwest::Client as HttpClient;
use serde::Deserialize;
use tracing::warn;

use crate::git_provider::github::client::Client as GitHubClient;
use crate::git_provider::gitlab::client::Client as GitLabClient;

use super::{FetchedTicket, TicketProvider, TicketRef};

/// Maximum characters of a ticket body we keep for the prompt. Mirrors
/// pr-agent's `MAX_TICKET_CHARACTERS = 10000` — a ticket past that is
/// truncated with a `[…truncated]` marker.
pub const TICKET_BODY_BUDGET: usize = 10_000;

/// Build the URL the GitHub client would send for an issue. Public so the
/// compliance tests can assert URL shape without spinning up a client.
pub fn github_issue_url(api_base: &str, owner: &str, repo: &str, number: u32) -> String {
    format!(
        "{}/repos/{}/{}/issues/{}",
        api_base.trim_end_matches('/'),
        owner,
        repo,
        number
    )
}

/// Build the URL the GitLab client would send for an issue. Public so the
/// compliance tests can assert URL shape without spinning up a client.
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

/// Fetch one ticket body from GitLab or GitHub.
///
/// `token` is the GitLab PAT / GitHub PAT — the same token `run_review`
/// already has for the MR/PR fetch.
///
/// On any non-2xx response the function returns a [`super::FetchedTicket`]
/// with `error` set and the caller's flow continues. The orchestrator
/// surfaces the error as a section note.
#[allow(clippy::expect_used)] // reqwest::Client::builder() with a timeout is infallible in practice
pub async fn fetch_ticket(
    provider: TicketProvider,
    api_base: &str,
    token: &str,
    ticket: &TicketRef,
) -> Result<FetchedTicket> {
    let http = HttpClient::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("reqwest client build is infallible (timeout)");
    let project = ticket.project.clone().unwrap_or_default();
    let resp = match provider {
        TicketProvider::GitHub => {
            // `project` is `owner/repo`.
            let mut parts = project.splitn(2, '/');
            let owner = parts.next().unwrap_or("");
            let repo = parts.next().unwrap_or("");
            if owner.is_empty() || repo.is_empty() {
                return Ok(fetch_failure(ticket, "missing owner/repo"));
            }
            let url = github_issue_url(api_base, owner, repo, ticket.iid);
            http.get(&url)
                .header("Authorization", format!("Bearer {token}"))
                .header("Accept", "application/vnd.github.v3+json")
                .header("User-Agent", "review-engine/0.10")
                .send()
                .await
        }
        TicketProvider::GitLab => {
            if project.is_empty() {
                return Ok(fetch_failure(ticket, "missing project path"));
            }
            let url = gitlab_issue_url(api_base, &project, ticket.iid);
            http.get(&url).header("PRIVATE-TOKEN", token).send().await
        }
    };
    let resp = match resp {
        Ok(r) => r,
        Err(e) => {
            warn!(
                url = %ticket.url,
                error = %e,
                "compliance ticket fetch transport failure"
            );
            return Ok(fetch_failure(ticket, &format!("transport error: {e}")));
        }
    };
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        let msg = format!("HTTP {status}: {}", truncate(&body, 200));
        warn!(
            url = %ticket.url,
            status = status.as_u16(),
            "compliance ticket fetch non-2xx"
        );
        return Ok(fetch_failure(ticket, &msg));
    }
    let json: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            warn!(url = %ticket.url, error = %e, "compliance ticket parse failure");
            return Ok(fetch_failure(ticket, &format!("parse error: {e}")));
        }
    };
    let parsed = match provider {
        TicketProvider::GitHub => parse_github(&json),
        TicketProvider::GitLab => parse_gitlab(&json),
    };
    let parsed = match parsed {
        Ok(p) => p,
        Err(e) => return Ok(fetch_failure(ticket, &format!("shape: {e}"))),
    };
    let body = truncate(&parsed.body, TICKET_BODY_BUDGET);
    Ok(FetchedTicket {
        ticket_ref: ticket.clone(),
        title: parsed.title,
        body,
        web_url: parsed.web_url,
        error: None,
    })
}

fn fetch_failure(ticket: &TicketRef, reason: &str) -> FetchedTicket {
    FetchedTicket {
        ticket_ref: ticket.clone(),
        title: String::new(),
        body: String::new(),
        web_url: None,
        error: Some(reason.to_string()),
    }
}

#[derive(Debug, Deserialize)]
struct GitHubIssue {
    title: Option<String>,
    body: Option<String>,
    html_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GitLabIssue {
    title: Option<String>,
    description: Option<String>,
    web_url: Option<String>,
}

struct Parsed {
    title: String,
    body: String,
    web_url: Option<String>,
}

fn parse_github(value: &serde_json::Value) -> anyhow::Result<Parsed> {
    let issue: GitHubIssue =
        serde_json::from_value(value.clone()).map_err(|e| anyhow::anyhow!("github issue shape: {e}"))?;
    Ok(Parsed {
        title: issue.title.unwrap_or_default(),
        body: issue.body.unwrap_or_default(),
        web_url: issue.html_url,
    })
}

fn parse_gitlab(value: &serde_json::Value) -> anyhow::Result<Parsed> {
    let issue: GitLabIssue =
        serde_json::from_value(value.clone()).map_err(|e| anyhow::anyhow!("gitlab issue shape: {e}"))?;
    Ok(Parsed {
        title: issue.title.unwrap_or_default(),
        body: issue.description.unwrap_or_default(),
        web_url: issue.web_url,
    })
}

fn truncate(body: &str, max_chars: usize) -> String {
    let count = body.chars().count();
    if count <= max_chars {
        return body.to_string();
    }
    let mut out: String = body.chars().take(max_chars).collect();
    out.push_str("\n\n[…truncated]");
    out
}

/// Route a ticket fetch to the right provider. The provider's own
/// `fetch_issue` method knows about the MR/PR-bound project; we let the
/// provider client do its own URL shaping rather than re-implementing it
/// here.
///
/// `mr_url` carries the original MR/PR URL whose scheme and host the
/// provider API base inherits. Without it the API base extraction would
/// guess `https` (matching the parser's `build_short_url`), which breaks
/// plain-HTTP self-hosted instances and wiremock-based tests.
///
/// Called from [`crate::compliance::run_for_review`].
pub async fn fetch_ticket_via_provider(mr_url: &str, ticket: &TicketRef) -> FetchedTicket {
    match ticket.provider {
        TicketProvider::GitLab => {
            // The provider client is project-scoped; we use the
            // `for_instance` unbound form (which returns a Client without
            // a project binding) so `fetch_issue` can carry the
            // ticket's own project path for cross-project refs.
            let token = std::env::var("GITLAB_TOKEN").unwrap_or_default();
            let instance_base = gitlab_api_base(mr_url);
            let client = GitLabClient::for_instance(&token, &instance_base);
            match client.fetch_issue(ticket.project.as_deref(), ticket.iid).await {
                Ok(t) => t,
                Err(e) => fetch_failure(ticket, &format!("provider fetch error: {e}")),
            }
        }
        TicketProvider::GitHub => {
            // The GitHub client parses owner/repo/number out of the URL;
            // we hand it a synthetic PR URL so the parser succeeds, then
            // pass the ticket's owner/repo to `fetch_issue` for the
            // actual API call.
            let token = std::env::var("GITHUB_TOKEN").unwrap_or_default();
            let url = github_pr_url_for_ticket(mr_url, ticket);
            match GitHubClient::new(&token, &url) {
                Ok(client) => {
                    let owner_repo = ticket.project.clone();
                    match client.fetch_issue(owner_repo.as_deref(), ticket.iid).await {
                        Ok(t) => t,
                        Err(e) => fetch_failure(ticket, &format!("provider fetch error: {e}")),
                    }
                }
                Err(e) => fetch_failure(ticket, &format!("provider client init: {e}")),
            }
        }
    }
}

/// Extract `scheme` (http | https) and `host` from an MR URL.
fn mr_url_scheme_host(mr_url: &str) -> (&str, &str) {
    if let Some(rest) = mr_url.strip_prefix("https://") {
        let host = rest.split('/').next().unwrap_or("");
        ("https", host)
    } else if let Some(rest) = mr_url.strip_prefix("http://") {
        let host = rest.split('/').next().unwrap_or("");
        ("http", host)
    } else {
        ("https", "")
    }
}

/// Build the GitLab API base from an MR URL.
fn gitlab_api_base(mr_url: &str) -> String {
    let (scheme, host) = mr_url_scheme_host(mr_url);
    if host.is_empty() {
        return "https://gitlab.example.com".to_string();
    }
    format!("{}://{}", scheme, host)
}

/// Best-effort PR-URL synthesis for the GitHub client: the GitHub client
/// parses owner/repo/number out of `…/owner/repo/pull/N`, but we want to
/// fetch an issue. We use the original MR URL's host/scheme so the
/// GitHub client targets the same instance the user passed in
/// (mock-friendly for self-hosted GitHub Enterprise).
fn github_pr_url_for_ticket(mr_url: &str, ticket: &TicketRef) -> String {
    let (scheme, host) = mr_url_scheme_host(mr_url);
    let project = ticket.project.clone().unwrap_or_default();
    let (owner, repo) = match project.split_once('/') {
        Some((o, r)) => (o, r),
        None => return mr_url.to_string(),
    };
    format!("{}://{}/{}/{}/pull/N", scheme, host, owner, repo)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ref_gitlab(iid: u32, project: &str) -> TicketRef {
        TicketRef {
            provider: TicketProvider::GitLab,
            project: Some(project.to_string()),
            iid,
            url: format!("https://gitlab.example.com/{project}/-/work_items/{iid}"),
            source: format!("!{iid}"),
        }
    }

    fn ref_github(iid: u32, project: &str) -> TicketRef {
        TicketRef {
            provider: TicketProvider::GitHub,
            project: Some(project.to_string()),
            iid,
            url: format!("https://github.com/{project}/issues/{iid}"),
            source: format!("#{iid}"),
        }
    }

    #[test]
    fn github_issue_url_encodes_owner_repo() {
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
    fn fetch_ticket_returns_failure_when_project_missing() {
        // We must call into the async fn synchronously here — `block_on`
        // would need a tokio setup, so use `tokio::runtime::Runtime::new()`.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let ticket = TicketRef {
            provider: TicketProvider::GitLab,
            project: None,
            iid: 1,
            url: "https://gitlab.example.com/-/work_items/1".into(),
            source: "!1".into(),
        };
        let result = rt.block_on(fetch_ticket(
            TicketProvider::GitLab,
            "https://gitlab.example.com/api/v4",
            "token",
            &ticket,
        ));
        let f = result.unwrap();
        assert!(!f.is_fetched());
        assert!(f.error.as_deref().unwrap_or("").contains("missing"));
    }

    #[test]
    fn fetch_ticket_github_returns_failure_when_owner_repo_malformed() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let ticket = ref_github(1, "only-owner");
        let result = rt.block_on(fetch_ticket(
            TicketProvider::GitHub,
            "https://api.github.com",
            "token",
            &ticket,
        ));
        let f = result.unwrap();
        assert!(!f.is_fetched());
        assert!(f.error.as_deref().unwrap_or("").contains("missing"));
    }

    // Mock-server coverage is in `tests/compliance_fetch.rs` (network
    // round-trip) — unit tests here cover only the URL builders and the
    // immediate-failure paths that do not need HTTP.

    #[test]
    fn encode_project_path_preserves_safe_chars() {
        assert_eq!(encode_project_path("g/p"), "g%2Fp");
        assert_eq!(encode_project_path("a-b_c.d~e/f"), "a-b_c.d~e%2Ff");
    }

    #[test]
    fn encode_project_path_handles_three_levels() {
        assert_eq!(encode_project_path("g/s/p"), "g%2Fs%2Fp");
    }

    // Use the unused-import lint to verify the symbol is exported.
    #[allow(dead_code)]
    fn _check_export(_t: FetchedTicket) {}

    // Quick parse-shape smoke tests: serde round-trips a known-good payload.
    #[test]
    fn parse_github_reads_title_and_body() {
        let json = serde_json::json!({
            "title": "t",
            "body": "b",
            "html_url": "u"
        });
        let p = parse_github(&json).unwrap();
        assert_eq!(p.title, "t");
        assert_eq!(p.body, "b");
        assert_eq!(p.web_url.as_deref(), Some("u"));
    }

    #[test]
    fn parse_gitlab_reads_title_and_description() {
        let json = serde_json::json!({
            "title": "t",
            "description": "b",
            "web_url": "u"
        });
        let p = parse_gitlab(&json).unwrap();
        assert_eq!(p.title, "t");
        assert_eq!(p.body, "b");
        assert_eq!(p.web_url.as_deref(), Some("u"));
    }

    #[test]
    fn parse_github_returns_error_on_missing_fields() {
        // An object that is not a map shape (e.g. a string) fails the serde
        // deserialise. Empty-object inputs are valid (every field is Option,
        // so they default to None); the parser fills the title/body with
        // empty strings and the section renders them.
        let json = serde_json::json!("not an object");
        assert!(parse_github(&json).is_err());
    }

    #[test]
    fn truncate_appends_marker_when_long() {
        let body = "x".repeat(100);
        let out = truncate(&body, 10);
        assert!(out.starts_with("xxxxxxxxxx"));
        assert!(out.contains("truncated"));
    }

    #[test]
    fn truncate_returns_input_when_short() {
        assert_eq!(truncate("short", 100), "short");
    }

    // Ensure the sample builder symbols stay referenced so clippy does not
    // flag the test helpers as dead — the lib path uses them indirectly via
    // `ref_gitlab` / `ref_github`.
    #[allow(dead_code)]
    fn _ref_builders_used(_t: TicketRef) {
        let _g = ref_gitlab(1, "g/p");
        let _h = ref_github(1, "o/r");
    }
}
