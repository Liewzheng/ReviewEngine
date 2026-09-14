//! RENG-33: review-URL routing for manual `gitlab_mr` submissions.
//!
//! A webhook payload carries GitLab's own `external_url`, which the webhook
//! path re-hosts onto the matched platform's reachable base before dispatch
//! (`crate::server::gitlab::rewrite_url_to_platform`). A manual submission
//! carries a URL the USER typed — usually the address their browser can open,
//! which is the same `external_url` and just as likely to be unreachable from
//! the server. This module applies the same rule to the manual path, with the
//! same matcher and the same rewrite function, so both trigger paths agree.
//!
//! Rules (see `docs/rest-api.md` §1 and `docs/integrations/gitlab.md`):
//!
//! 1. **Matched** — a configured git platform whose `base_url` (the address
//!    that appears in payloads and pasted URLs) or, when set,
//!    `internal_base_url` has the URL's scheme-less `host[:port]` identity
//!    (`GitPlatformConfig::matches_review_url`; host case-insensitive, an
//!    explicitly written default port folded to absent, otherwise the port is
//!    compared strictly). The review then fetches the URL re-hosted onto that
//!    platform's review-time base: `internal_base_url` when set, else
//!    `base_url`. First match in config order wins; a matched platform's
//!    configured target is always trusted (so a GitLab the server really does
//!    reach on a local address keeps working).
//! 2. **Unmatched local host** — no platform matched AND the URL host is a
//!    well-known local address (`localhost`, any `127.0.0.0/8`, `0.0.0.0`,
//!    `::1`, `::`). Inside a container those name the container itself, so the
//!    review could only fail later with an opaque "Failed to send GET": the
//!    submission is rejected up front with an actionable 400 instead.
//! 3. **Unmatched otherwise** — fetched exactly as submitted (unchanged).
//!
//! The rewritten URL is what the review fetches AND what the task record
//! stores as its MR URL; the URL the user typed is not preserved (it is
//! logged at submit time with the platform it matched).

use crate::models::GitPlatformConfig;

/// The routing decision for one `gitlab_mr` submission URL.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum MrUrlRoute {
    /// No platform owns the URL: fetch it exactly as submitted.
    Unchanged,
    /// `platform` owns the URL: fetch it re-hosted onto that platform's
    /// review-time base (`internal_base_url`, else `base_url`).
    Rewritten { url: String, platform: String },
}

/// Route one manual `gitlab_mr` URL (rule list in the module docs).
///
/// `Err(message)` is the actionable rejection for a URL whose host the server
/// cannot reach and that no platform claims (rule 2); callers return it as a
/// 400. Pure: no I/O, no credential use — safe to call before any policy gate.
pub(crate) fn route_gitlab_mr_url(platforms: &[GitPlatformConfig], url: &str) -> Result<MrUrlRoute, String> {
    if let Some(platform) = crate::models::find_git_platform_for_review_url(platforms, url) {
        // Same rewrite the webhook path applies: the submitted path (query
        // included) re-hosted onto the platform's reachable base. Fail-safe:
        // an unparseable target keeps the submitted URL verbatim.
        let rewritten =
            crate::server::gitlab::rewrite_url_to_platform(url, crate::server::gitlab::review_base_url(platform));
        return Ok(MrUrlRoute::Rewritten {
            url: rewritten,
            platform: platform.name.clone(),
        });
    }

    let host = reqwest::Url::parse(url.trim())
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_string));
    match host.filter(|host| is_server_local_host(host)) {
        Some(host) => Err(unreachable_local_host_message(&host)),
        None => Ok(MrUrlRoute::Unchanged),
    }
}

/// True for addresses that name the server itself rather than a GitLab host
/// the server could dial: `localhost` / `*.localhost` (RFC 6761), any
/// `127.0.0.0/8` loopback address, the unspecified addresses `0.0.0.0` / `::`,
/// and IPv6 loopback `::1` (bracketed forms included — `Url::host_str` keeps
/// the brackets). A hostname that merely RESOLVES to a local address is out of
/// scope: this is a syntactic check, no DNS.
fn is_server_local_host(host: &str) -> bool {
    let host = host.trim();
    let host = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(host);
    let host = host.to_ascii_lowercase();
    if host == "localhost" || host.ends_with(".localhost") {
        return true;
    }
    if let Ok(v4) = host.parse::<std::net::Ipv4Addr>() {
        return v4.is_loopback() || v4.is_unspecified();
    }
    if let Ok(v6) = host.parse::<std::net::Ipv6Addr>() {
        return v6.is_loopback() || v6.is_unspecified();
    }
    false
}

/// The rejection message for rule 2. Names the offending host, why it cannot
/// work, and the two ways out: submit the address the server reaches (the
/// container-local alias of the host is `host.docker.internal`), or configure
/// a git platform entry covering this host so the URL is rewritten for us.
fn unreachable_local_host_message(host: &str) -> String {
    format!(
        "gitlab_mr url host `{host}` is unreachable from the review server: a local address names the \
         server itself (inside a container, the container), and no configured git platform matches it. \
         Use the GitLab address this server can reach — in Docker that is usually `host.docker.internal` \
         (e.g. `http://host.docker.internal:8929/group/project/-/merge_requests/1`) — or configure a git \
         platform entry for this host with `baseUrl` = the address you browse to and \
         `internalBaseUrl` (`internal_base_url`) = the address the server reaches, so MR URLs on this host \
         are rewritten onto the reachable base automatically"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn platform(name: &str, base_url: &str, internal_base_url: &str) -> GitPlatformConfig {
        GitPlatformConfig {
            name: name.to_string(),
            platform_type: "gitlab".to_string(),
            base_url: base_url.to_string(),
            internal_base_url: internal_base_url.to_string(),
            token: "glpat-platform".to_string(),
            webhook_secret: String::new(),
            webhook_signing_secret: String::new(),
            allowed_projects: Vec::new(),
        }
    }

    #[test]
    fn matched_platform_rewrites_onto_internal_base_url() {
        // NAS shape: the user pastes the external :8443 address, the server
        // reaches the instance on the internal 443.
        let platforms = vec![platform(
            "nas",
            "https://gitlab.islet.space:8443",
            "https://gitlab.islet.space",
        )];
        assert_eq!(
            route_gitlab_mr_url(
                &platforms,
                "https://gitlab.islet.space:8443/group/proj/-/merge_requests/2"
            )
            .unwrap(),
            MrUrlRoute::Rewritten {
                url: "https://gitlab.islet.space/group/proj/-/merge_requests/2".to_string(),
                platform: "nas".to_string(),
            }
        );
    }

    #[test]
    fn matched_platform_without_internal_falls_back_to_base_url() {
        // Local testbed shape: the pasted (external) host is rewritten onto
        // the container-reachable base_url.
        let platforms = vec![platform(
            "testbed",
            "http://localhost:8929",
            "http://host.docker.internal:8929",
        )];
        assert_eq!(
            route_gitlab_mr_url(
                &platforms,
                "http://localhost:8929/review-lab/demo-app/-/merge_requests/1"
            )
            .unwrap(),
            MrUrlRoute::Rewritten {
                url: "http://host.docker.internal:8929/review-lab/demo-app/-/merge_requests/1".to_string(),
                platform: "testbed".to_string(),
            }
        );

        let platforms = vec![platform("testbed", "http://localhost:8929", "")];
        assert_eq!(
            route_gitlab_mr_url(
                &platforms,
                "http://localhost:8929/review-lab/demo-app/-/merge_requests/1"
            )
            .unwrap(),
            MrUrlRoute::Rewritten {
                url: "http://localhost:8929/review-lab/demo-app/-/merge_requests/1".to_string(),
                platform: "testbed".to_string(),
            },
            "a matched platform is trusted: its configured target is what the server reaches"
        );
    }

    #[test]
    fn rewrite_preserves_path_and_query_and_normalises_port_and_trailing_slash() {
        let platforms = vec![platform(
            "nas",
            "https://gitlab.example.com:8443",
            "https://gitlab.example.com",
        )];
        // A trailing slash on the configured base is irrelevant to the match
        // (only host[:port] is compared) and is stripped by the rewrite.
        let platforms_trailing_slash = vec![platform(
            "nas",
            "https://gitlab.example.com:8443/",
            "https://gitlab.example.com/",
        )];
        assert_eq!(
            route_gitlab_mr_url(
                &platforms_trailing_slash,
                "https://gitlab.example.com:8443/g/p/-/merge_requests/9"
            )
            .unwrap(),
            MrUrlRoute::Rewritten {
                url: "https://gitlab.example.com/g/p/-/merge_requests/9".to_string(),
                platform: "nas".to_string(),
            }
        );
        // Explicit default port on the submitted URL identifies the same
        // instance as the port-less configured base.
        let platforms_default_port = vec![platform("web", "https://gitlab.com", "https://gitlab.internal")];
        assert_eq!(
            route_gitlab_mr_url(
                &platforms_default_port,
                "https://gitlab.com:443/group/proj/-/merge_requests/9"
            )
            .unwrap(),
            MrUrlRoute::Rewritten {
                url: "https://gitlab.internal/group/proj/-/merge_requests/9".to_string(),
                platform: "web".to_string(),
            }
        );
        // Query string survives the rewrite.
        assert_eq!(
            route_gitlab_mr_url(
                &platforms,
                "https://gitlab.example.com:8443/group/proj/-/merge_requests/9?note_id=3#note_3"
            )
            .unwrap(),
            MrUrlRoute::Rewritten {
                url: "https://gitlab.example.com/group/proj/-/merge_requests/9?note_id=3".to_string(),
                platform: "nas".to_string(),
            }
        );
    }

    #[test]
    fn unmatched_reachable_host_passes_through_unchanged() {
        let platforms = vec![platform("testbed", "http://gitlab.internal:8929", "")];
        // A host no platform claims and that is not a local address: the
        // server may well reach it (e.g. gitlab.com), so it is submitted as-is.
        assert_eq!(
            route_gitlab_mr_url(&platforms, "https://gitlab.com/group/proj/-/merge_requests/1").unwrap(),
            MrUrlRoute::Unchanged
        );
        assert_eq!(
            route_gitlab_mr_url(&[], "https://gitlab.com/group/proj/-/merge_requests/1").unwrap(),
            MrUrlRoute::Unchanged
        );
        // Malformed URLs are `Unchanged` here — the 422 URL validator owns them.
        assert_eq!(route_gitlab_mr_url(&[], "not-a-url").unwrap(), MrUrlRoute::Unchanged);
    }

    #[test]
    fn local_host_without_matching_platform_is_rejected_with_the_fix() {
        let platforms = vec![platform("testbed", "http://host.docker.internal:8929", "")];
        for url in [
            "http://localhost:8929/group/proj/-/merge_requests/1",
            "http://127.0.0.1:8929/group/proj/-/merge_requests/1",
            "http://127.0.0.2:8929/group/proj/-/merge_requests/1",
            "http://0.0.0.0:8929/group/proj/-/merge_requests/1",
            "http://gitlab.localhost:8929/group/proj/-/merge_requests/1",
        ] {
            let err = route_gitlab_mr_url(&platforms, url).expect_err("a local host must be rejected");
            assert!(err.contains("unreachable from the review server"), "{err}");
            assert!(
                err.contains("host.docker.internal"),
                "must name the container alias: {err}"
            );
            assert!(
                err.contains("internalBaseUrl") && err.contains("internal_base_url"),
                "must name the platform field to configure: {err}"
            );
        }
    }

    #[test]
    fn is_server_local_host_classifies_addresses() {
        for local in [
            "localhost",
            "LOCALHOST",
            "gitlab.localhost",
            "127.0.0.1",
            "127.9.9.9",
            "0.0.0.0",
            "[::1]",
            "::1",
            "::",
            "[0:0:0:0:0:0:0:1]",
        ] {
            assert!(
                is_server_local_host(local),
                "{local} must be treated as the server itself"
            );
        }
        for remote in [
            "gitlab.com",
            "host.docker.internal",
            "10.0.0.5",
            "192.168.1.10",
            "[::2]",
        ] {
            assert!(!is_server_local_host(remote), "{remote} must not be treated as local");
        }
    }
}
