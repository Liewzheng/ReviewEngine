//! Ticket reference extraction from an MR/PR description (M40 / RENG).
//!
//! Mirrors pr-agent's `extract_gitlab_ticket_references` (host gate, project
//! parser, iid extraction, dedup, max-tickets cap) and the matching GitHub
//! path. We re-implement the parsing semantics in Rust rather than
//! translating the Python line-for-line — the patterns and host gate are
//! the load-bearing parts; everything else is data shaping.
//!
//! Recognised forms (parity with pr-agent's
//! `ticket_pr_compliance_check.py::GITLAB_TICKET_PATTERN` / `GITHUB_TICKET_PATTERN`):
//!
//! GitLab:
//! * `#N` (local — same project as the MR; bounded to ≤ `MAX_SHORTHAND_ISSUE_DIGITS` digits)
//! * `org/repo#N` (cross-project)
//! * `<https://gitlab.example.com/group/project/-/work_items/N>`
//! * `<https://gitlab.example.com/group/project/-/issues/N>`
//!
//! GitLab MR sigil `!N` is **deliberately ignored**: pr-agent's GitLab pattern
//! also matches `!N`, but `!` is the MR-reference syntax on GitLab —
//! fetching "issue #N" via the issues endpoint when the description refers
//! to a merge request gives an unrelated object (or a 404). When the
//! description names an MR with `!N`, the reviewer wants the MR's
//! description, not an issue's; supporting that requires the merge-requests
//! endpoint, which is out of scope for M40. The renderer's notes path
//! documents the choice.
//!
//! GitHub:
//! * `#N` (local — same repo as the PR; ≤ `MAX_SHORTHAND_ISSUE_DIGITS` digits)
//! * `org/repo#N`
//! * `<https://github.com/org/repo/issues/N>`

use regex::Regex;

use super::{TicketProvider, TicketRef};

/// pr-agent's bound on bare `#N` / `!N` shorthand
/// (`ticket_pr_compliance_check.py:25-29`): a 7-digit "#1234567" is as
/// likely an error code as an issue, so the shorthand only fires when
/// the digit run fits this width.
pub const MAX_SHORTHAND_ISSUE_DIGITS: usize = 6;

/// Three compiled patterns per provider, each guarded separately so a
/// false positive on one form does not bleed into another. The look-behind
/// `(?<![A-Za-z0-9_])` we want for the local shorthand is not available
/// in the `regex` crate; instead we post-check the byte immediately
/// preceding each local-shorthand match.
struct Patterns {
    url: Regex,
    project: Regex,
    local: Regex,
}

#[allow(clippy::expect_used)] // regex strings are constant; tested at startup
fn gitlab_patterns() -> &'static Patterns {
    static RE: std::sync::OnceLock<Patterns> = std::sync::OnceLock::new();
    RE.get_or_init(|| Patterns {
        url: Regex::new(r"https?://[^\s<>)]+?/-/(?:work_items|issues)/\d+").expect("gitlab url regex"),
        project: Regex::new(r"[\w./-]+?#(\d+)\b").expect("gitlab project regex"),
        local: Regex::new(r"#(\d+)\b").expect("gitlab local regex"),
    })
}

#[allow(clippy::expect_used)] // regex strings are constant; tested at startup
fn github_patterns() -> &'static Patterns {
    static RE: std::sync::OnceLock<Patterns> = std::sync::OnceLock::new();
    RE.get_or_init(|| Patterns {
        url: Regex::new(r"https?://[^\s<>)]+?/issues/\d+").expect("github url regex"),
        project: Regex::new(r"[\w./-]+?#(\d+)\b").expect("github project regex"),
        local: Regex::new(r"#(\d+)\b").expect("github local regex"),
    })
}

/// Extract ticket references from `description`.
///
/// `mr_project` is the GitLab project / GitHub owner-repo of the MR itself
/// — needed to backfill the local-shorthand refs (`#N`) into a canonical
/// URL. `provider_host` is the host the MR was opened on; refs whose URL
/// is on a different host are dropped (the cross-host guard
/// pr-agent's `extract_gitlab_ticket_references` enforces).
///
/// GitLab's `!N` is the MR-reference sigil, NOT a ticket reference —
/// it is intentionally NOT parsed (see module docs). GitHub's `#N`
/// shorthand covers the same role on the GitHub side.
pub fn extract_ticket_refs(
    description: &str,
    provider: TicketProvider,
    mr_project: &str,
    provider_host: &str,
) -> Vec<TicketRef> {
    if description.trim().is_empty() {
        return Vec::new();
    }
    let patterns = match provider {
        TicketProvider::GitLab => gitlab_patterns(),
        TicketProvider::GitHub => github_patterns(),
    };
    let mut out = Vec::new();
    let bytes = description.as_bytes();
    // (1) URL form.
    for m in patterns.url.find_iter(description) {
        let url = m.as_str();
        // Strip the trailing punctuation pr-agent also strips, so an
        // URL at the end of a sentence (`…see <https://gitlab/foo!1>.`)
        // is not parsed with a trailing `.` glued onto the host.
        let cleaned =
            url.trim_end_matches(|c: char| matches!(c, '.' | ',' | ';' | ':' | '!' | '?' | ')' | '>' | '\'' | '"'));
        if !url_host_matches(cleaned, provider_host) {
            continue;
        }
        let (project, iid, source) = parse_url_into(cleaned, provider);
        let project = project.or_else(|| Some(mr_project.to_string()));
        let Some(iid) = iid else { continue };
        out.push(TicketRef {
            provider,
            project,
            iid,
            url: cleaned.to_string(),
            source,
        });
    }
    // (2) Cross-project shorthand: `org/repo!N` (GitLab) or `org/repo#N`
    // (GitHub). The match gives us only the full match — recover the
    // project from everything before the trailing digits.
    for m in patterns.project.find_iter(description) {
        let full = m.as_str();
        // Find the start of the trailing digit sequence.
        let mut digit_start = full.len();
        for (i, c) in full.char_indices().rev() {
            if c.is_ascii_digit() {
                digit_start = i;
            } else {
                break;
            }
        }
        let project_str = full[..digit_start].trim_end_matches(|c: char| matches!(c, '!' | '#'));
        let slash_count = project_str.matches('/').count();
        if slash_count != 1 {
            // Must be exactly `owner/repo` for GitHub or `group/sub` for
            // GitLab — anything else (e.g. `foo!bar!42`) is ambiguous.
            continue;
        }
        let iid: u32 = match full[digit_start..].parse() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let url = build_short_url(provider, provider_host, project_str, iid);
        out.push(TicketRef {
            provider,
            project: Some(project_str.to_string()),
            iid,
            url,
            source: full.to_string(),
        });
    }
    // (3) Local shorthand: `#N` for both GitLab and GitHub (pr-agent
    // parity — `!N` is GitLab's MR sigil and is intentionally NOT
    // matched here; see the module docs). Require that the byte
    // immediately before the match is NOT a word char — the same guard a
    // look-behind would give us — and that the digit run fits the
    // shorthand budget.
    for m in patterns.local.find_iter(description) {
        let start = m.start();
        if start > 0
            && bytes[start - 1] != b' '
            && bytes[start - 1] != b'\t'
            && bytes[start - 1] != b'\n'
            && bytes[start - 1] != b'\r'
            && bytes[start - 1] != b'>'
            && bytes[start - 1] != b'<'
            && (bytes[start - 1] as char).is_alphanumeric()
        {
            // The character before is alphanumeric — likely a hex fragment
            // (`abc#12345`) or an identifier continuation — skip.
            continue;
        }
        let digits = &description[m.start() + 1..m.end()];
        if digits.len() > MAX_SHORTHAND_ISSUE_DIGITS {
            // pr-agent: a bare #1234567 is as likely an error code as an
            // issue, so shorthand is bounded to ≤ 6 digits.
            continue;
        }
        let iid: u32 = match digits.parse() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let url = build_short_url(provider, provider_host, mr_project, iid);
        out.push(TicketRef {
            provider,
            project: Some(mr_project.to_string()),
            iid,
            url,
            source: description[start..m.end()].to_string(),
        });
    }
    // Dedup identical refs in source order, so the caller does not have to.
    let mut seen = std::collections::HashSet::new();
    out.retain(|r| seen.insert((r.provider, r.project.clone(), r.iid, r.url.clone())));
    out
}

/// True when the URL's host equals `provider_host` (with optional userinfo
/// stripped), matching pr-agent's same-host gate.
fn url_host_matches(url: &str, provider_host: &str) -> bool {
    if provider_host.is_empty() {
        return true;
    }
    let Some(scheme_end) = url.find("://") else {
        return false;
    };
    let after_scheme = &url[scheme_end + 3..];
    let host_part = after_scheme
        .split('/')
        .next()
        .unwrap_or("")
        .split('@')
        .next_back()
        .unwrap_or("");
    // The MR-URL host keeps an optional port (`gitlab.example.com:8443`),
    // the URL embedded in the description may also keep one — compare them
    // directly.
    host_part == provider_host
}

/// Parse a `<url>` group into `(project, iid, source)`.
///
/// `project` is `None` when the URL does not carry one (the GitLab
/// work-item URL is host-scoped; we default to the MR's project when the
/// caller passes it in — see [`extract_ticket_refs`]).
fn parse_url_into(url: &str, provider: TicketProvider) -> (Option<String>, Option<u32>, String) {
    // Strip scheme.
    let after_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let mut path_parts = after_scheme.splitn(2, '/');
    let _host = path_parts.next().unwrap_or("");
    let path = path_parts.next().unwrap_or("");
    // Strip fragment / query.
    let path = path.split('?').next().unwrap_or(path);
    let path = path.split('#').next().unwrap_or(path);
    let source = url.to_string();
    match provider {
        TicketProvider::GitLab => {
            // Path shape: group/project/-/work_items/N (or /-/issues/N).
            let segments: Vec<&str> = path.split('/').collect();
            // Find the work_items/ or issues/ marker.
            let marker_idx = segments.iter().position(|s| *s == "work_items" || *s == "issues");
            let Some(marker_idx) = marker_idx else {
                return (None, None, source);
            };
            // Project is segments[..marker_idx-1] joined by '/' — skipping
            // the "-/" marker. Path: "group", "project", "-", "work_items", "N"
            // → project = "group/project", iid = N.
            if marker_idx < 3 {
                return (None, None, source);
            }
            let project = segments[..marker_idx - 1].join("/");
            let iid = match segments.get(marker_idx + 1).and_then(|s| s.parse::<u32>().ok()) {
                Some(v) => v,
                None => return (None, None, source),
            };
            (Some(project), Some(iid), source)
        }
        TicketProvider::GitHub => {
            // Path shape: owner/repo/issues/N.
            let segments: Vec<&str> = path.split('/').collect();
            if segments.len() < 4 || segments[2] != "issues" {
                return (None, None, source);
            }
            let project = format!("{}/{}", segments[0], segments[1]);
            let iid = match segments.get(3).and_then(|s| s.parse::<u32>().ok()) {
                Some(v) => v,
                None => return (None, None, source),
            };
            (Some(project), Some(iid), source)
        }
    }
}

/// Build a short URL for shorthand forms (no scheme-and-path re-encoding —
/// `provider_host` already includes the port when applicable).
fn build_short_url(provider: TicketProvider, provider_host: &str, project: &str, iid: u32) -> String {
    if provider_host.is_empty() {
        // No host means we cannot build a URL — the caller should drop this
        // ref. Returning an empty URL lets the dedup key still distinguish
        // different `(project, iid)` pairs in the rare "no host" case
        // (useful for tests); the rendering path skips refs with no URL.
        return String::new();
    }
    match provider {
        TicketProvider::GitLab => format!("https://{}/{}/-/work_items/{}", provider_host, project, iid),
        TicketProvider::GitHub => format!("https://{}/{}/issues/{}", provider_host, project, iid),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refs(description: &str, provider: TicketProvider, mr_project: &str, host: &str) -> Vec<TicketRef> {
        extract_ticket_refs(description, provider, mr_project, host)
    }

    #[test]
    fn gitlab_local_hash_extracts_iid() {
        // P1-2 / M39 §7: the form the user's own MR descriptions use
        // (`Closes #N`) is the GitLab local sigil. pr-agent parity.
        let out = refs(
            "Implements #73 — SIRENA-84",
            TicketProvider::GitLab,
            "isletspace/sirena",
            "gitlab.islet.space:8443",
        );
        assert_eq!(out.len(), 1, "single local #N reference: {out:?}");
        assert_eq!(out[0].iid, 73);
        assert_eq!(out[0].project.as_deref(), Some("isletspace/sirena"));
        assert!(out[0].url.contains("gitlab.islet.space:8443"));
        assert!(out[0].url.contains("/-/work_items/73"));
    }

    #[test]
    fn gitlab_cross_project_with_hash() {
        // P1-2: GitLab cross-project — pr-agent uses `#N` here too.
        let out = refs(
            "Closes isletspace/sirena#73",
            TicketProvider::GitLab,
            "other/group",
            "gitlab.islet.space:8443",
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].iid, 73);
        assert_eq!(out[0].project.as_deref(), Some("isletspace/sirena"));
    }

    #[test]
    fn gitlab_bang_sigil_is_ignored() {
        // P1-2: GitLab `!N` is the MR sigil. Fetching it via the issues
        // endpoint returns an unrelated object (or 404); pr-agent also
        // ignores it. The compliance sub-step therefore matches only the
        // `#N` form on GitLab.
        let out = refs(
            "Closes !73",
            TicketProvider::GitLab,
            "isletspace/sirena",
            "gitlab.islet.space:8443",
        );
        assert!(
            out.is_empty(),
            "GitLab !N is the MR sigil and is intentionally not matched: {out:?}"
        );
    }

    #[test]
    fn gitlab_work_item_url_extracts_project_and_iid() {
        let out = refs(
            "Closes <https://gitlab.islet.space:8443/isletspace/sirena/-/work_items/73>",
            TicketProvider::GitLab,
            "other/group",
            "gitlab.islet.space:8443",
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].iid, 73);
        assert_eq!(out[0].project.as_deref(), Some("isletspace/sirena"));
    }

    #[test]
    fn gitlab_work_item_url_on_other_host_is_dropped() {
        let out = refs(
            "Closes <https://gitlab.example.com/other/group/-/work_items/9>",
            TicketProvider::GitLab,
            "isletspace/sirena",
            "gitlab.islet.space:8443",
        );
        assert!(out.is_empty(), "different host should be dropped: {out:?}");
    }

    #[test]
    fn gitlab_multiple_refs_dedup_and_order_preserved() {
        // P1-2: `!N` is intentionally NOT a GitLab shorthand here (the MR
        // sigil); `#N` and the cross-project form are.
        let out = refs(
            "Closes #75, isletspace/sirena#76, #77",
            TicketProvider::GitLab,
            "isletspace/sirena",
            "gitlab.islet.space:8443",
        );
        assert_eq!(out.len(), 3, "three refs matched: {out:?}");
        let mut iids: Vec<u32> = out.iter().map(|r| r.iid).collect();
        iids.sort();
        assert_eq!(iids, vec![75, 76, 77]);
    }

    #[test]
    fn github_local_hash_extracts_iid() {
        let out = refs("Closes #73", TicketProvider::GitHub, "isletspace/sirena", "github.com");
        assert_eq!(out.len(), 1, "single local #N reference: {out:?}");
        assert_eq!(out[0].iid, 73);
        assert_eq!(out[0].project.as_deref(), Some("isletspace/sirena"));
        assert_eq!(out[0].url, "https://github.com/isletspace/sirena/issues/73");
    }

    #[test]
    fn github_cross_repo_with_hash() {
        let out = refs(
            "Fixes isletspace/sirena#42",
            TicketProvider::GitHub,
            "other/repo",
            "github.com",
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].iid, 42);
        assert_eq!(out[0].project.as_deref(), Some("isletspace/sirena"));
        assert_eq!(out[0].url, "https://github.com/isletspace/sirena/issues/42");
    }

    #[test]
    fn github_issue_url_extracts_owner_repo_number() {
        let out = refs(
            "Resolves <https://github.com/isletspace/sirena/issues/42>",
            TicketProvider::GitHub,
            "other/repo",
            "github.com",
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].iid, 42);
        assert_eq!(out[0].project.as_deref(), Some("isletspace/sirena"));
    }

    #[test]
    fn github_issue_url_on_other_host_is_dropped() {
        let out = refs(
            "Closes <https://gitlab.example.com/other/repo/-/work_items/9>",
            TicketProvider::GitHub,
            "isletspace/sirena",
            "github.com",
        );
        assert!(out.is_empty(), "different host should be dropped: {out:?}");
    }

    #[test]
    fn github_hash_within_digit_budget_is_matched() {
        // P2-4: pr-agent's MAX_SHORTHAND_ISSUE_DIGITS = 6 caps bare `#N`.
        let out = refs("fixes #123456", TicketProvider::GitHub, "owner/repo", "github.com");
        assert_eq!(out.len(), 1, "6 digits still fits the budget");
        assert_eq!(out[0].iid, 123_456);
    }

    #[test]
    fn github_hash_over_digit_budget_is_rejected() {
        // P2-4: 7-digit `#N` is treated as an error code, not an issue
        // reference — pr-agent parity.
        let out = refs("fixes #1234567", TicketProvider::GitHub, "owner/repo", "github.com");
        assert!(out.is_empty(), "7+ digit shorthand is rejected: {out:?}");
    }

    #[test]
    fn github_hash_following_alphanumeric_is_rejected() {
        // Hex fragment in a commit message: `abc#123` is not a ticket ref
        // even though `#123` is a 3-digit shorthand.
        let out = refs(
            "commit abc#123 landed",
            TicketProvider::GitHub,
            "owner/repo",
            "github.com",
        );
        assert!(out.is_empty(), "alphanumeric before #N skips the shorthand: {out:?}");
    }

    #[test]
    fn empty_description_extracts_nothing() {
        let out = refs("", TicketProvider::GitHub, "owner/repo", "github.com");
        assert!(out.is_empty());
    }

    #[test]
    fn no_refs_extracts_nothing() {
        let out = refs(
            "Just a regular description with no ticket mentions.",
            TicketProvider::GitHub,
            "owner/repo",
            "github.com",
        );
        assert!(out.is_empty());
    }

    #[test]
    fn multiple_refs_are_deduplicated() {
        let out = refs(
            "Closes #42, fixes #42, and #42 again",
            TicketProvider::GitHub,
            "owner/repo",
            "github.com",
        );
        let unique: std::collections::HashSet<_> = out.iter().map(|r| (r.provider, r.iid)).collect();
        assert_eq!(out.len(), unique.len(), "dedup keeps only one #42");
    }

    #[test]
    fn trailing_punctuation_does_not_break_url_extraction() {
        let out = refs(
            "see <https://gitlab.example.com/g/p/-/work_items/9>.",
            TicketProvider::GitLab,
            "g/p",
            "gitlab.example.com",
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].iid, 9);
    }

    #[test]
    fn url_host_matches_handles_userinfo() {
        assert!(url_host_matches(
            "https://user:pass@gitlab.example.com/g/p/-/work_items/9",
            "gitlab.example.com"
        ));
    }

    #[test]
    fn url_host_matches_handles_port_in_provider_host() {
        assert!(url_host_matches(
            "https://gitlab.example.com:8443/g/p/-/work_items/9",
            "gitlab.example.com:8443"
        ));
    }

    #[test]
    fn url_host_matches_returns_false_for_scheme_less_input() {
        assert!(!url_host_matches("not a url", "gitlab.example.com"));
    }

    #[test]
    fn url_host_matches_returns_true_when_provider_host_is_empty() {
        // Empty host = trust everything (matches pr-agent's behaviour on
        // unparseable MR URLs).
        assert!(url_host_matches("https://anywhere.example/x/y", ""));
    }

    #[test]
    fn gitlab_local_hash_works_when_mr_project_empty() {
        // P1-2: same as the old !N variant, but for the #N form which the
        // parser now matches.
        let out = refs("#9", TicketProvider::GitLab, "", "gitlab.example.com");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].iid, 9);
        // Empty mr_project leaves the URL's project segment empty; the
        // dedup key is (provider, project, iid, url) so two empty-MR-project
        // refs of the same iid collide.
        assert_eq!(out[0].url, "https://gitlab.example.com//-/work_items/9");
    }

    #[test]
    fn github_url_in_gitlab_provider_is_dropped() {
        let out = refs(
            "see <https://github.com/owner/repo/issues/1>",
            TicketProvider::GitLab,
            "g/p",
            "gitlab.example.com",
        );
        // The GitLab regex does NOT match `/issues/N` (no `work_items/`
        // or `/-/issues/N` segment), so nothing is parsed.
        assert!(out.is_empty());
    }
}
