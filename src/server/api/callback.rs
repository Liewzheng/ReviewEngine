//! Webhook completion callbacks for REST review tasks.
//!
//! When a review submitted via the REST API carries a `webhook` URL, the
//! outcome is POSTed to that URL once the task finishes (success or failure).
//! Delivery is fire-and-forget: callbacks run in a background task with a
//! 10s timeout and failures are only logged, never fail the review task.
//!
//! SSRF protection (docs/rest-api.md §1 "Webhook 回调 URL 校验"): the URL is
//! validated at enqueue time by the submit/rerun handlers (failure → `400`),
//! and re-validated at send time (DNS results can change between enqueue and
//! completion). Redirects are never followed — a 3xx response is terminal —
//! so a validated target cannot bounce the callback into a disallowed one.
//!
//! @module review-engine: part of the CodeReview Board virtual engineering team
use std::net::{IpAddr, Ipv6Addr};
use std::time::Duration;

use serde::Serialize;
use uuid::Uuid;

/// Maximum time to wait for the user's webhook endpoint before giving up.
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(10);

/// JSON body POSTed to the user's webhook URL when a review task finishes.
#[derive(Debug, Serialize)]
pub struct CallbackPayload {
    pub task_id: Uuid,
    /// `"completed"` or `"failed"`.
    pub status: &'static str,
    /// Short human-readable report summary (present on success).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Error message (present on failure).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Unwrap IPv4-mapped IPv6 addresses (`::ffff:127.0.0.1`) so the range checks
/// below cannot be bypassed by dressing an IPv4 target in IPv6 notation.
fn normalize_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(IpAddr::V6(v6)),
        v4 => v4,
    }
}

/// fc00::/7 (unique-local) membership; `std` has no stable predicate for it.
fn is_unique_local_v6(ip: Ipv6Addr) -> bool {
    (ip.segments()[0] & 0xfe00) == 0xfc00
}

/// Named predicate for the `http` scheme exemption (docs/rest-api.md §1):
/// `http` callbacks are allowed only for loopback / private-network
/// deployments — `127.0.0.0/8`, `::1`, `10.0.0.0/8`, `172.16.0.0/12`,
/// `192.168.0.0/16`, `fc00::/7`.
pub fn is_loopback_or_private(ip: IpAddr) -> bool {
    match normalize_ip(ip) {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private(),
        IpAddr::V6(v6) => v6.is_loopback() || is_unique_local_v6(v6),
    }
}

/// Always-rejected targets, for both `http` and `https` and applied to the
/// literal host IP and every DNS-resolved address: link-local / cloud
/// metadata (`169.254.0.0/16` incl. `169.254.169.254`, `fe80::/10`),
/// unspecified (`0.0.0.0/8`, `::`), plus multicast/reserved ranges that can
/// never be legitimate callback endpoints.
pub fn is_blocked_callback_addr(ip: IpAddr) -> bool {
    match normalize_ip(ip) {
        IpAddr::V4(v4) => v4.is_link_local() || v4.octets()[0] == 0 || v4.is_multicast() || v4.is_broadcast(),
        IpAddr::V6(v6) => v6.is_unicast_link_local() || v6.is_unspecified() || v6.is_multicast(),
    }
}

/// Parse the URL host into a literal IP, if it is one. The `url` crate
/// already normalizes exotic IPv4 forms (`127.1`, `0x7f.1`, octal) into
/// canonical dotted-quad in `host_str`, so those bypasses land here; IPv6
/// literals arrive bracketed (`[::1]`).
fn host_ip_literal(host: &str) -> Option<IpAddr> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Some(ip);
    }
    let inner = host.strip_prefix('[')?.strip_suffix(']')?;
    inner.parse::<Ipv6Addr>().ok().map(IpAddr::V6)
}

/// Scheme + address policy decision, shared by literal-IP and DNS-resolved
/// paths (kept separate from I/O so tests can drive the full table without
/// mocking DNS).
fn validate_scheme_and_ips(scheme: &str, ips: &[IpAddr]) -> Result<(), String> {
    for ip in ips {
        if is_blocked_callback_addr(*ip) {
            return Err(format!(
                "target address {ip} is in a blocked range (link-local/metadata/unspecified)"
            ));
        }
    }
    match scheme {
        "https" => Ok(()),
        "http" => {
            if ips.iter().all(|ip| is_loopback_or_private(*ip)) {
                Ok(())
            } else {
                Err("http is only allowed for loopback/private targets; use https for public hosts".to_string())
            }
        }
        other => Err(format!("unsupported scheme '{other}': only http/https are allowed")),
    }
}

/// Resolve a non-literal host via DNS (async, tokio `getaddrinfo`).
async fn resolve_host_ips(host: &str, port: u16) -> Result<Vec<IpAddr>, String> {
    let addrs = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| format!("DNS resolution failed for host '{host}': {e}"))?;
    let ips: Vec<IpAddr> = addrs.map(|sa| sa.ip()).collect();
    if ips.is_empty() {
        return Err(format!("host '{host}' resolved to no addresses"));
    }
    Ok(ips)
}

/// Full SSRF validation of a webhook callback URL (async: includes DNS).
///
/// - scheme allowlist: `https`, or `http` only when every target address is
///   loopback/private ([`is_loopback_or_private`]);
/// - link-local / metadata / unspecified addresses are always rejected
///   ([`is_blocked_callback_addr`]), for the literal host IP and every
///   DNS-resolved address (guards against DNS names pointing at the metadata
///   endpoint at validation time);
/// - DNS failure or an empty resolution is rejected (fail-closed).
///
/// Returns `Err(reason)` suitable for a `400 {"error": "invalid webhook url:
/// <reason>"}` response.
pub async fn validate_callback_url(url: &str) -> Result<(), String> {
    let parsed = reqwest::Url::parse(url).map_err(|e| format!("unparseable url: {e}"))?;
    let scheme = parsed.scheme().to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return Err(format!("unsupported scheme '{scheme}': only http/https are allowed"));
    }
    let host = parsed.host_str().ok_or_else(|| "missing host".to_string())?.to_string();
    let port = parsed.port_or_known_default().unwrap_or(443);
    let ips = match host_ip_literal(&host) {
        Some(ip) => vec![ip],
        None => resolve_host_ips(&host, port).await?,
    };
    validate_scheme_and_ips(&scheme, &ips)
}

/// Fire-and-forget: POST the task outcome to `webhook` in the background.
///
/// The URL was validated at enqueue time; it is re-validated here (async,
/// inside the spawned task) because DNS answers can change between enqueue
/// and task completion. Delivery failures only produce a `warn` log entry —
/// the review task itself is never affected.
pub fn spawn_callback(
    webhook: Option<String>,
    task_id: Uuid,
    status: &'static str,
    summary: Option<String>,
    error: Option<String>,
) {
    let Some(url) = webhook else { return };
    tokio::spawn(async move {
        if let Err(reason) = validate_callback_url(&url).await {
            tracing::warn!("Skipping review webhook for task {task_id}: re-validation failed: {reason}");
            return;
        }
        if let Err(e) = send_callback(&url, task_id, status, summary, error).await {
            tracing::warn!("Review webhook callback failed for task {task_id}: {e}");
        }
    });
}

/// POST the callback payload once, with a 10s timeout.
///
/// Redirects are not followed (`redirect::Policy::none()`): a 3xx is treated
/// as a terminal failure rather than re-validating and chasing the target —
/// the simplest policy that can never bounce a validated callback into a
/// disallowed address.
async fn send_callback(
    url: &str,
    task_id: Uuid,
    status: &'static str,
    summary: Option<String>,
    error: Option<String>,
) -> anyhow::Result<()> {
    let client = reqwest::Client::builder()
        .timeout(CALLBACK_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let payload = CallbackPayload {
        task_id,
        status,
        summary,
        error,
    };
    let response = client.post(url).json(&payload).send().await?;
    if response.status().is_redirection() {
        anyhow::bail!(
            "webhook endpoint returned redirect (HTTP {}); redirects are not followed",
            response.status()
        );
    }
    if !response.status().is_success() {
        anyhow::bail!("webhook endpoint returned HTTP {}", response.status());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // ─── address predicates ─────────────────────────────────────

    #[test]
    fn loopback_private_predicate_table() {
        let ok = [
            "127.0.0.1",
            "127.0.1.9",
            "10.0.0.1",
            "10.255.255.254",
            "172.16.0.1",
            "172.31.255.254",
            "192.168.0.1",
            "::1",
            "fc00::1",
            "fd00::1",
            // IPv4-mapped IPv6 must be unwrapped before the checks.
            "::ffff:127.0.0.1",
            "::ffff:10.1.2.3",
        ];
        for ip in ok {
            assert!(
                is_loopback_or_private(ip.parse().unwrap()),
                "{ip} must be loopback/private"
            );
        }
        let not = [
            "93.184.216.34",
            "8.8.8.8",
            "172.15.0.1",
            "172.32.0.1",
            "11.0.0.1",
            "2606:4700::1",
        ];
        for ip in not {
            assert!(
                !is_loopback_or_private(ip.parse().unwrap()),
                "{ip} must not be loopback/private"
            );
        }
    }

    #[test]
    fn blocked_addr_predicate_table() {
        let blocked = [
            "169.254.169.254", // cloud metadata
            "169.254.0.1",
            "0.0.0.0",
            "0.1.2.3", // whole 0/8
            "fe80::1",
            "fe80::dead:beef",
            "::",
            "255.255.255.255",
            "224.0.0.1", // multicast
            "ff02::1",
            // IPv4-mapped forms of the above.
            "::ffff:169.254.169.254",
            "::ffff:0.0.0.0",
        ];
        for ip in blocked {
            assert!(is_blocked_callback_addr(ip.parse().unwrap()), "{ip} must be blocked");
        }
        let allowed = ["93.184.216.34", "127.0.0.1", "10.0.0.1", "::1", "2606:4700::1"];
        for ip in allowed {
            assert!(
                !is_blocked_callback_addr(ip.parse().unwrap()),
                "{ip} must not be blocked"
            );
        }
    }

    // ─── scheme + resolved-IP policy (DNS-free seam) ────────────

    #[test]
    fn policy_table() {
        let public: IpAddr = "93.184.216.34".parse().unwrap();
        let loopback: IpAddr = "127.0.0.1".parse().unwrap();
        let private: IpAddr = "192.168.1.10".parse().unwrap();
        let metadata: IpAddr = "169.254.169.254".parse().unwrap();

        // https public ok; http public rejected.
        assert!(validate_scheme_and_ips("https", &[public]).is_ok());
        assert!(validate_scheme_and_ips("http", &[public]).is_err());
        // http loopback / private ok.
        assert!(validate_scheme_and_ips("http", &[loopback]).is_ok());
        assert!(validate_scheme_and_ips("http", &[private]).is_ok());
        // https to loopback/private is fine too.
        assert!(validate_scheme_and_ips("https", &[loopback]).is_ok());
        assert!(validate_scheme_and_ips("https", &[private]).is_ok());
        // Metadata/link-local blocked under both schemes.
        assert!(validate_scheme_and_ips("https", &[metadata]).is_err());
        assert!(validate_scheme_and_ips("http", &[metadata]).is_err());
        // One blocked address among several good ones fails the whole set.
        assert!(validate_scheme_and_ips("https", &[public, metadata]).is_err());
        // http requires ALL resolved addresses loopback/private (a DNS name
        // resolving to both a private and a public IP is not an http target).
        assert!(validate_scheme_and_ips("http", &[loopback, private]).is_ok());
        assert!(validate_scheme_and_ips("http", &[loopback, public]).is_err());
        // Non-http(s) schemes rejected outright.
        assert!(validate_scheme_and_ips("ftp", &[loopback]).is_err());
        assert!(validate_scheme_and_ips("gopher", &[loopback]).is_err());
    }

    // ─── full async validation (literal IPs + localhost DNS) ────

    #[tokio::test]
    async fn validation_table_literal_ips() {
        // https public ok
        assert!(validate_callback_url("https://93.184.216.34/hook?x=1").await.is_ok());
        // http public rejected
        let err = validate_callback_url("http://93.184.216.34/hook").await.unwrap_err();
        assert!(err.contains("loopback/private"), "unexpected: {err}");
        // http loopback ok
        assert!(validate_callback_url("http://127.0.0.1:8080/hook").await.is_ok());
        assert!(validate_callback_url("http://[::1]:8080/hook").await.is_ok());
        // http private ok
        assert!(validate_callback_url("http://10.1.2.3/hook").await.is_ok());
        assert!(validate_callback_url("http://172.16.5.4/hook").await.is_ok());
        assert!(validate_callback_url("http://192.168.1.1/hook").await.is_ok());
        assert!(validate_callback_url("http://[fd00::9]/hook").await.is_ok());
        // link-local / metadata rejected under both schemes
        let err = validate_callback_url("https://169.254.169.254/latest/meta-data")
            .await
            .unwrap_err();
        assert!(err.contains("blocked range"), "unexpected: {err}");
        assert!(validate_callback_url("http://169.254.169.254/").await.is_err());
        // 0.0.0.0 rejected
        assert!(validate_callback_url("http://0.0.0.0:9000/").await.is_err());
        // fe80::/10 rejected
        assert!(validate_callback_url("http://[fe80::1]/hook").await.is_err());
        // exotic IPv4 spellings are normalized by url parsing then blocked
        assert!(validate_callback_url("http://2130706433/").await.is_ok()); // 127.0.0.1 as u32 → loopback http ok
                                                                            // bad scheme / unparseable / missing host rejected
        assert!(validate_callback_url("ftp://example.com/hook").await.is_err());
        assert!(validate_callback_url("file:///etc/passwd").await.is_err());
        assert!(validate_callback_url("gopher://127.0.0.1/").await.is_err());
        assert!(validate_callback_url("not-a-url").await.is_err());
        assert!(validate_callback_url("").await.is_err());
        assert!(validate_callback_url("http:///no-host").await.is_err());
    }

    #[tokio::test]
    async fn dns_name_resolving_to_loopback() {
        // `localhost` resolves to 127.0.0.1/::1 without external DNS.
        // Decision per doc: loopback is not in the always-blocked set, so
        // https to a name resolving to loopback is allowed; http is allowed
        // because every resolved address is loopback/private.
        assert!(validate_callback_url("https://localhost/hook").await.is_ok());
        assert!(validate_callback_url("http://localhost:9/hook").await.is_ok());
    }

    #[tokio::test]
    async fn dns_failure_is_rejected() {
        let err = validate_callback_url("https://nonexistent.invalid./hook")
            .await
            .unwrap_err();
        assert!(err.contains("DNS resolution failed"), "unexpected: {err}");
    }

    // ─── delivery ───────────────────────────────────────────────

    #[tokio::test]
    async fn test_spawn_callback_with_invalid_url_does_not_panic() {
        spawn_callback(
            Some("ftp://example.com/hook".to_string()),
            Uuid::new_v4(),
            "completed",
            Some("summary".to_string()),
            None,
        );
        spawn_callback(None, Uuid::new_v4(), "failed", None, Some("err".to_string()));
    }

    #[tokio::test]
    async fn test_spawn_callback_skips_blocked_target() {
        // A URL that fails send-time re-validation must not be delivered.
        spawn_callback(
            Some("https://169.254.169.254/hook".to_string()),
            Uuid::new_v4(),
            "completed",
            None,
            None,
        );
        // Give the spawned task a chance to (not) run; nothing to assert on
        // the network side, the test passes as long as no panic/deadlock.
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    #[tokio::test]
    async fn test_send_callback_posts_payload() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/hook"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let task_id = Uuid::new_v4();
        let url = format!("{}/hook", server.uri());
        send_callback(&url, task_id, "completed", Some("2 findings".to_string()), None)
            .await
            .unwrap();

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["task_id"], task_id.to_string());
        assert_eq!(body["status"], "completed");
        assert_eq!(body["summary"], "2 findings");
        assert!(body.get("error").is_none());
    }

    #[tokio::test]
    async fn test_send_callback_failure_status_is_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let result = send_callback(&server.uri(), Uuid::new_v4(), "failed", None, Some("boom".to_string())).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_send_callback_redirect_is_terminal_and_not_followed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/hook"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", "http://169.254.169.254/"))
            .mount(&server)
            .await;

        let url = format!("{}/hook", server.uri());
        let result = send_callback(&url, Uuid::new_v4(), "completed", None, None).await;
        let err = result.unwrap_err().to_string();
        assert!(err.contains("redirect"), "unexpected error: {err}");

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1, "the redirect must not be followed");
    }

    #[tokio::test]
    async fn test_spawn_callback_delivers_in_background() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let task_id = Uuid::new_v4();
        spawn_callback(Some(server.uri()), task_id, "failed", None, Some("timeout".to_string()));

        // The callback runs on a spawned task; poll briefly for delivery.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if server.received_requests().await.unwrap().len() == 1 {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "callback was not delivered");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let body: serde_json::Value =
            serde_json::from_slice(&server.received_requests().await.unwrap()[0].body).unwrap();
        assert_eq!(body["task_id"], task_id.to_string());
        assert_eq!(body["status"], "failed");
        assert_eq!(body["error"], "timeout");
    }
}
