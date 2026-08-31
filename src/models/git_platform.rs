//! Git platform instance configuration (multi-instance support).
//!
//! A [`GitPlatformConfig`] describes one reachable Git host the server can
//! review on and receive webhooks from. Only `gitlab` is implemented today,
//! but the schema carries a `type` field so `gitea` / `gitee` entries can
//! slot into the same structure in a later release without a migration.
//!
//! Serde field names are snake_case: instances of this struct persist to
//! `ui-state.toml`, where the codebase convention (see [`crate::models::LLMConfig`])
//! is snake_case. The camelCase REST contract lives in the UI layer
//! (`UiGitPlatformConfig` in `server::api::config::types`), which converts
//! to and from this struct.

use serde::{Deserialize, Serialize};

/// One configured Git host (e.g. a self-hosted GitLab instance).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitPlatformConfig {
    /// Unique, user-chosen instance name; the merge key for `PUT /config`.
    #[serde(default)]
    pub name: String,
    /// Platform kind. Only `"gitlab"` is implemented today.
    #[serde(rename = "type", default = "default_platform_type")]
    pub platform_type: String,
    /// Instance base URL (e.g. `https://gitlab.example.com`, port allowed).
    #[serde(default)]
    pub base_url: String,
    /// API token (live secret — the UI only ever sees the `***` mask).
    #[serde(default)]
    pub token: String,
    /// Legacy webhook secret (`X-Gitlab-Token` header verification).
    #[serde(default)]
    pub webhook_secret: String,
    /// GitLab 19+ webhook signing secret (`whsec_...`, Standard Webhooks).
    #[serde(default)]
    pub webhook_signing_secret: String,
}

fn default_platform_type() -> String {
    "gitlab".to_string()
}

impl GitPlatformConfig {
    /// True when the entry carries at least one webhook verification
    /// credential and can therefore act as a webhook receiver for its host.
    /// Token-only entries exist for review routing (REST `gitlab_mr`
    /// credential resolution) and deliberately do NOT take over webhook
    /// verification for their host — the runtime default keeps applying.
    pub fn has_webhook_verification(&self) -> bool {
        !self.webhook_secret.is_empty() || !self.webhook_signing_secret.is_empty()
    }

    /// True when `url` belongs to this instance: the scheme-less
    /// `host[:port]` identity matches (host compared case-insensitively).
    /// Port matching is strict — explicit differing ports never match; the
    /// only fold is an explicitly written port equal to the URL scheme's
    /// default, so `https://gitlab.com` matches `https://gitlab.com:443/...`.
    pub fn matches_url(&self, url: &str) -> bool {
        match (host_port(&self.base_url), host_port(url)) {
            (Some(platform), Some(target)) => platform == target,
            _ => false,
        }
    }
}

/// Find the configured platform serving `url` for INBOUND purposes (which
/// instance's credentials verify this webhook payload).
///
/// Strict first: scheme-less `host[:port]` match against each entry's
/// `base_url` (see [`GitPlatformConfig::matches_url`]). When no strict match
/// exists, fall back to a host-only match — a self-hosted instance is often
/// reachable at a different port than its advertised `external_url` (e.g.
/// GitLab behind an `https://host:8443` port mapping while the server is
/// configured against `https://host` on 443). The fallback considers only
/// entries with webhook verification credentials (see
/// [`GitPlatformConfig::has_webhook_verification`]): token-only entries
/// cannot verify an inbound payload, so they must neither win the fallback
/// nor make it ambiguous. The fallback hits only when the URL's host
/// identifies EXACTLY one verification-capable entry; zero or multiple host
/// matches (or an unparseable URL) yield `None` — never guess.
///
/// Callers that SEND credentials to the URL's endpoint (REST `gitlab_mr`
/// token resolution) must use [`find_git_platform_for_url_strict`] instead:
/// folding the port there would widen where a configured token is sent.
pub fn find_git_platform_for_url<'a>(platforms: &'a [GitPlatformConfig], url: &str) -> Option<&'a GitPlatformConfig> {
    if let Some(strict) = find_git_platform_for_url_strict(platforms, url) {
        return Some(strict);
    }
    let target_host = host_port(url)?.0;
    let mut host_matches = platforms.iter().filter(|p| {
        p.has_webhook_verification() && host_port(&p.base_url).is_some_and(|(host, _)| host == target_host)
    });
    let hit = host_matches.next()?;
    if host_matches.next().is_some() {
        return None;
    }
    Some(hit)
}

/// Strict variant of [`find_git_platform_for_url`]: scheme-less `host[:port]`
/// match only, no host-only fallback. Use when the matched entry's
/// credentials are sent OUTBOUND to the URL's host:port (the configured
/// token must never flow to a port that was not explicitly configured).
/// Returns `None` when no entry matches or either URL fails to parse.
pub fn find_git_platform_for_url_strict<'a>(
    platforms: &'a [GitPlatformConfig],
    url: &str,
) -> Option<&'a GitPlatformConfig> {
    platforms.iter().find(|p| p.matches_url(url))
}

/// Normalise a URL to its scheme-less `(host, port)` identity.
///
/// The host is lowercased (DNS is case-insensitive). Port matching is
/// strict: the explicit port is kept as written, so `http://host:8929` and
/// `http://host:9999` are different instances. The only fold is an explicit
/// port equal to the SCHEME's default (80/443), treated as absent — so a
/// side that omits the port matches a side that writes the default port
/// out. Note `Url::port_or_known_default()` cannot express this: it returns
/// the explicit port whenever one is written, which would fold EVERY port
/// to absent and make port matching non-strict.
///
/// URLs without a host (or that fail to parse) yield `None` and simply
/// never match.
fn host_port(url: &str) -> Option<(String, Option<u16>)> {
    let parsed = reqwest::Url::parse(url.trim()).ok()?;
    let host = parsed.host_str()?.to_ascii_lowercase();
    let scheme_default_port = match parsed.scheme() {
        "http" => Some(80u16),
        "https" => Some(443u16),
        _ => None,
    };
    let port = match parsed.port() {
        Some(p) if scheme_default_port == Some(p) => None,
        other => other,
    };
    Some((host, port))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn platform(base_url: &str) -> GitPlatformConfig {
        GitPlatformConfig {
            name: "testbed".to_string(),
            platform_type: "gitlab".to_string(),
            base_url: base_url.to_string(),
            token: "glpat-platform".to_string(),
            webhook_secret: "wh-secret".to_string(),
            webhook_signing_secret: String::new(),
        }
    }

    #[test]
    fn matches_url_host_and_explicit_port() {
        let p = platform("http://host.docker.internal:8929");
        assert!(p.matches_url("http://host.docker.internal:8929/group/proj/-/merge_requests/1"));
        // Scheme is ignored: scheme-less comparison.
        assert!(p.matches_url("https://host.docker.internal:8929/group/proj"));
        // Host is case-insensitive.
        assert!(p.matches_url("http://HOST.docker.INTERNAL:8929/group/proj"));
        // Different port → different instance.
        assert!(!p.matches_url("http://host.docker.internal:9999/group/proj"));
        // Missing port → not equal to an explicit-port instance.
        assert!(!p.matches_url("http://host.docker.internal/group/proj"));
        // Different host → no match.
        assert!(!p.matches_url("http://other.internal:8929/group/proj"));
    }

    #[test]
    fn matches_url_default_port_folds_to_absent() {
        let p = platform("https://gitlab.com");
        assert!(p.matches_url("https://gitlab.com/group/proj/-/merge_requests/42"));
        // An explicitly written default port identifies the same instance.
        assert!(p.matches_url("https://gitlab.com:443/group/proj/-/merge_requests/42"));
        assert!(!p.matches_url("https://gitlab.com:8443/group/proj"));
    }

    #[test]
    fn matches_url_handles_garbage_and_trailing_slash() {
        let p = platform("http://gitlab.internal:8929/");
        assert!(p.matches_url("http://gitlab.internal:8929/group/proj"));
        assert!(!p.matches_url("not-a-url"));
        assert!(!p.matches_url(""));
        let unparseable = platform("not a url at all");
        assert!(!unparseable.matches_url("http://gitlab.internal:8929/x"));
    }

    #[test]
    fn find_platform_for_url_picks_first_match() {
        let platforms = vec![
            platform("http://gitlab-a.internal:8929"),
            GitPlatformConfig {
                name: "second".to_string(),
                ..platform("http://gitlab-b.internal")
            },
        ];
        let hit = find_git_platform_for_url(&platforms, "http://gitlab-b.internal/g/p/-/merge_requests/1");
        assert_eq!(hit.map(|p| p.name.as_str()), Some("second"));
        assert!(find_git_platform_for_url(&platforms, "http://unrelated.internal/x").is_none());
        assert!(find_git_platform_for_url(&[], "http://gitlab-a.internal:8929/x").is_none());
    }

    #[test]
    fn find_platform_strict_match_wins_over_host_fallback() {
        let platforms = vec![
            platform("https://gitlab.internal:8443"),
            GitPlatformConfig {
                name: "plain".to_string(),
                ..platform("https://gitlab.internal")
            },
        ];
        // Port matches exactly → strict hit, even though a host-only
        // fallback would be ambiguous (two entries share the host).
        let hit = find_git_platform_for_url(&platforms, "https://gitlab.internal:8443/g/p/-/merge_requests/1");
        assert_eq!(hit.map(|p| p.name.as_str()), Some("testbed"));
        let hit = find_git_platform_for_url(&platforms, "https://gitlab.internal/g/p");
        assert_eq!(hit.map(|p| p.name.as_str()), Some("plain"));
    }

    #[test]
    fn find_platform_falls_back_to_unique_host_match() {
        // Platform reachable on 443, but the webhook payload carries the
        // external_url port mapping (:8443).
        let platforms = vec![platform("https://gitlab.internal")];
        let hit = find_git_platform_for_url(&platforms, "https://gitlab.internal:8443/g/p/-/merge_requests/1");
        assert_eq!(hit.map(|p| p.name.as_str()), Some("testbed"));
        // Reverse direction folds too: explicit-port platform, port-less URL.
        let platforms = vec![platform("https://gitlab.internal:8443")];
        let hit = find_git_platform_for_url(&platforms, "https://gitlab.internal/g/p");
        assert_eq!(hit.map(|p| p.name.as_str()), Some("testbed"));
    }

    #[test]
    fn find_platform_ambiguous_host_fallback_yields_none() {
        let platforms = vec![
            platform("https://gitlab.internal:8443"),
            GitPlatformConfig {
                name: "plain".to_string(),
                ..platform("https://gitlab.internal")
            },
        ];
        // No strict match for :9443 and the host alone is ambiguous → None.
        assert!(find_git_platform_for_url(&platforms, "https://gitlab.internal:9443/g/p").is_none());
    }

    fn token_only(base_url: &str, name: &str) -> GitPlatformConfig {
        GitPlatformConfig {
            name: name.to_string(),
            webhook_secret: String::new(),
            ..platform(base_url)
        }
    }

    #[test]
    fn find_platform_fallback_ignores_token_only_entries() {
        // Same host: a token-only entry (review routing only) and an entry
        // with webhook verification credentials. Without the verification
        // filter the fallback would be ambiguous → None → 403.
        let platforms = vec![
            token_only("https://gitlab.internal:8443", "review-only"),
            platform("https://gitlab.internal"),
        ];
        let hit = find_git_platform_for_url(&platforms, "https://gitlab.internal:9443/g/p");
        assert_eq!(hit.map(|p| p.name.as_str()), Some("testbed"));
    }

    #[test]
    fn find_platform_fallback_token_only_sole_host_match_yields_none() {
        // The only entry on this host cannot verify webhooks → None rather
        // than a hit whose signature check would fail anyway.
        let platforms = vec![token_only("https://gitlab.internal", "review-only")];
        assert!(find_git_platform_for_url(&platforms, "https://gitlab.internal:9443/g/p").is_none());
    }

    #[test]
    fn find_platform_unknown_host_stays_none() {
        let platforms = vec![platform("https://gitlab.internal")];
        assert!(find_git_platform_for_url(&platforms, "https://elsewhere.example.com:8443/g/p").is_none());
        assert!(find_git_platform_for_url(&platforms, "not-a-url").is_none());
    }

    #[test]
    fn find_platform_strict_never_folds_ports() {
        // The outbound-credential path stays strict: where the fallback
        // would fold a unique host, the strict variant yields None.
        let platforms = vec![platform("https://gitlab.internal")];
        let url = "https://gitlab.internal:8443/g/p/-/merge_requests/1";
        assert!(find_git_platform_for_url(&platforms, url).is_some());
        assert!(find_git_platform_for_url_strict(&platforms, url).is_none());
        // Strict still matches exactly (default port folds as before).
        let hit = find_git_platform_for_url_strict(&platforms, "https://gitlab.internal:443/g/p");
        assert_eq!(hit.map(|p| p.name.as_str()), Some("testbed"));
    }

    #[test]
    fn has_webhook_verification_semantics() {
        let mut p = platform("http://gitlab.internal");
        assert!(p.has_webhook_verification(), "webhook_secret counts");
        p.webhook_secret = String::new();
        assert!(
            !p.has_webhook_verification(),
            "token-only entry does not receive webhooks"
        );
        p.webhook_signing_secret = "whsec_abc".to_string();
        assert!(p.has_webhook_verification(), "signing secret counts");
    }

    #[test]
    fn toml_round_trip_uses_snake_case_and_type_key() {
        let p = platform("http://gitlab.internal:8929");
        let text = toml::to_string(&p).unwrap();
        assert!(
            text.contains("base_url = \"http://gitlab.internal:8929\""),
            "got: {text}"
        );
        assert!(text.contains("type = \"gitlab\""), "got: {text}");
        assert!(!text.contains("baseUrl"), "camelCase must not leak into TOML: {text}");
        let back: GitPlatformConfig = toml::from_str(&text).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn toml_type_defaults_to_gitlab_when_absent() {
        let parsed: GitPlatformConfig = toml::from_str(
            r#"
name = "x"
base_url = "http://gitlab.internal"
"#,
        )
        .unwrap();
        assert_eq!(parsed.platform_type, "gitlab");
        assert!(parsed.token.is_empty());
    }
}
