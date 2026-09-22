//! Connectivity probe for LLM providers.
//!
//! Shared by the server provider-test endpoints
//! (`POST /api/v1/llm/providers/{id}/test`, `POST /api/v1/config/test`) and
//! the CLI `reng config provider test` command, so all three probe a
//! provider in exactly the same way.
//!
//! What the probe reports is what the provider cards show in their error
//! column, so the request it sends has to be the request the provider actually
//! accepts (RENG-66). Both the auth header ([`auth_headers`]) and the
//! model-list URL ([`models_url`]) therefore come from the SAME resolution the
//! completion path uses — [`crate::llm::provider::provider_kind`] (which folds
//! case, so `"Anthropic"` is probed and completed as Anthropic alike) and
//! [`crate::llm::provider::anthropic_api_base`] — because a card that reads
//! `healthy` must be a card whose completions are attempted the same way.
//!
//! One known exception, tracked separately (F-2) and deliberately not folded in
//! here: an Ollama card configured by bare host is probed at `/v1/models` while
//! its OpenAI-compatible completion path still posts to `{base}/chat/completions`.
//!
//! A failure is classified ([`ProbeFailureKind`]) so a rejected key and an
//! unreachable provider no longer read the same way.

use anyhow::Result;

use crate::models::LLMConfig;

/// The result of a successful probe, carrying the base URL that was
/// actually probed so callers can show the user exactly where the stored
/// key was sent.
#[derive(Debug, Clone)]
pub struct ProbeOutcome {
    /// The effective base URL after applying the well-known defaults.
    pub resolved_base: String,
}

/// How long a probe waits for the provider before calling it unreachable.
pub const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// The API version a probe announces to Anthropic. Anthropic rejects a
/// request that omits it; the completion path
/// ([`crate::llm::provider::AnthropicProvider`]) sends the same value.
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Which failure a probe hit (RENG-66).
///
/// The classes exist because they ask the user to do different things: a
/// credential problem is fixed by changing the key, an unreachable provider by
/// changing `api_base` or the network, and a provider-side error by waiting.
/// Before this, all of them reached the health page as prose — `HTTP 401` or
/// `error sending request for url (…)` — which is why a normal Anthropic key
/// and a dead provider looked alike.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeFailureKind {
    /// The provider answered 401/403: it is reachable and it rejected the API
    /// key. Nothing about the network needs fixing.
    Auth,
    /// Nothing answered: DNS, connect, TLS, or timeout. The key was never
    /// checked.
    Unreachable,
    /// The provider answered 4xx without blaming this request's credentials or
    /// its rate: most often a 404, meaning `api_base` does not point at an
    /// OpenAI-compatible API (or at the wrong version of it).
    BadEndpoint,
    /// The provider answered but the answer blames ITS OWN state, not this
    /// request and not the address: a 5xx (unhealthy), or a 429 (rate-limited,
    /// P2-1). Neither is a credential problem and neither is `api_base`.
    ProviderError,
}

/// A failed probe, classified (RENG-66).
///
/// It is an `Error` so it keeps travelling the `anyhow` chain every caller
/// already walks (the health page shows `e.to_string()`); `kind` is there for a
/// caller that wants to branch on the class instead of matching on prose.
#[derive(Debug, Clone)]
pub struct ProbeFailure {
    /// The class of failure.
    pub kind: ProbeFailureKind,
    /// The provider name as configured (the name that chose the headers).
    pub provider: String,
    /// The URL the probe asked for a model list.
    pub url: String,
    /// The HTTP status when the provider answered; `None` when nothing did.
    pub status: Option<u16>,
    /// The transport error and its causes — the part that tells "connection
    /// refused" from "dns error" from "timed out". Empty when the provider
    /// answered with a status.
    pub detail: String,
}

impl std::fmt::Display for ProbeFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let provider = &self.provider;
        let url = &self.url;
        match self.kind {
            ProbeFailureKind::Auth => write!(
                f,
                "authentication failed (HTTP {}) at {url}: provider \"{provider}\" rejected the API key. \
                 The provider is reachable — check the key itself, not the network.",
                self.status_text()
            ),
            ProbeFailureKind::Unreachable => write!(
                f,
                "service unreachable at {url}: {}. The provider never answered, so the key was never \
                 checked — check api_base, DNS and the network path to \"{provider}\".",
                self.detail
            ),
            ProbeFailureKind::BadEndpoint => write!(
                f,
                "provider \"{provider}\" is reachable but serves no model list at {url} (HTTP {}). \
                 Check api_base — it should be the API root of an OpenAI-compatible endpoint.",
                self.status_text()
            ),
            ProbeFailureKind::ProviderError => {
                // P2-1: a 429 asks for patience, not for a fix. Without this
                // branch a rate-limited provider would be described as an
                // unhealthy one, and its `api_base` (the one thing that is
                // right) would read as the suspect.
                let advice = if self.status == Some(429) {
                    "Not a credential problem — the provider is reachable and rate-limiting \
                     this client; retry later."
                } else {
                    "Not a credential problem — the provider itself is unhealthy; retry later."
                };
                write!(
                    f,
                    "provider \"{provider}\" is reachable but answered an error (HTTP {}) at {url}. {advice}",
                    self.status_text()
                )
            }
        }
    }
}

impl std::error::Error for ProbeFailure {}

impl ProbeFailure {
    /// The status with its reason phrase (`401 Unauthorized`), or `no response`
    /// for the transport class.
    fn status_text(&self) -> String {
        match self.status {
            Some(code) => match reqwest::StatusCode::from_u16(code) {
                Ok(status) => status.to_string(),
                Err(_) => format!("HTTP {code}"),
            },
            None => "no response".to_string(),
        }
    }

    /// A failure that never produced a response.
    fn unreachable(cfg: &LLMConfig, url: String, err: &reqwest::Error) -> Self {
        let detail = if err.is_timeout() {
            format!("no response within {}s", PROBE_TIMEOUT.as_secs())
        } else {
            transport_detail(err)
        };
        Self {
            kind: ProbeFailureKind::Unreachable,
            provider: cfg.provider.clone(),
            url,
            status: None,
            detail,
        }
    }

    /// A failure the provider itself reported.
    fn from_status(cfg: &LLMConfig, url: String, status: reqwest::StatusCode) -> Self {
        let kind = if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            ProbeFailureKind::Auth
        } else if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            // P2-1: a 429 is the PROVIDER throttling this client, not a wrong
            // address. Reading it as `BadEndpoint` told the user to check
            // `api_base` — the one thing that was right.
            ProbeFailureKind::ProviderError
        } else if status.is_client_error() {
            ProbeFailureKind::BadEndpoint
        } else {
            ProbeFailureKind::ProviderError
        };
        Self {
            kind,
            provider: cfg.provider.clone(),
            url,
            status: Some(status.as_u16()),
            detail: String::new(),
        }
    }
}

/// Read a transport error together with its causes.
///
/// `reqwest::Error`'s own `Display` names the URL but not always the cause, and
/// the cause is the whole message for the user: `connection refused` (wrong
/// host/port), `dns error` (bad name), `invalid peer certificate` (TLS). The
/// chain is flattened, de-duplicated, and joined so the reported detail is the
/// useful part rather than `error sending request for url (…)` alone.
fn transport_detail(err: &reqwest::Error) -> String {
    use std::error::Error as _;

    let mut parts = vec![err.to_string()];
    let mut source = err.source();
    while let Some(cause) = source {
        let text = cause.to_string();
        if !text.is_empty() && !parts.iter().any(|part| part == &text) {
            parts.push(text);
        }
        source = cause.source();
    }
    parts.join(": ")
}

/// Whether a provider name is Anthropic's API (RENG-66).
///
/// [`crate::llm::provider::provider_kind`] answers this — the SAME function
/// [`crate::llm::provider::ProviderRegistry::from_configs`] routes completions
/// with — so the probe and the completion path cannot drift apart on case
/// (RENG-66 r2). Any other name is an OpenAI-compatible endpoint, a proxy in
/// front of Claude included.
fn is_anthropic(provider: &str) -> bool {
    crate::llm::provider::provider_kind(provider) == crate::llm::provider::ProviderKind::Anthropic
}

/// The auth header(s) a provider expects, as `(name, value)` pairs (RENG-66).
///
/// `anthropic` gets Anthropic's pair and everything else a bearer token — the
/// same split the completion path makes, resolved by the same function, so a
/// card that reads `healthy` is a card whose completions carry the same
/// credentials.
pub fn auth_headers(provider: &str, api_key: &str) -> Vec<(&'static str, String)> {
    if is_anthropic(provider) {
        vec![
            ("x-api-key", api_key.to_string()),
            ("anthropic-version", ANTHROPIC_VERSION.to_string()),
        ]
    } else {
        vec![("Authorization", format!("Bearer {api_key}"))]
    }
}

/// The URL a provider serves its model list at, given its effective base
/// (RENG-66).
///
/// An OpenAI-compatible base normally names its version — `…/v1`, or `…/v1beta`
/// for the Gemini-style endpoints — and then only `/models` is appended. The
/// Anthropic base goes through [`crate::llm::provider::anthropic_api_base`], the
/// same normalization `AnthropicProvider::complete` applies, so the probe and the
/// completion path agree on the versioned base too (the builtin catalog prefills
/// `https://api.anthropic.com/v1`; RENG-66 r2 / F-1). Ollama's host-style base is
/// prefixed `/v1` here as well, but its completion path does not normalize yet —
/// that mismatch is F-2, tracked separately and deliberately left alone. Every
/// other host is used as written, because `https://api.deepseek.com/models` and
/// friends are valid that way — adding `/v1` there would break the probe.
pub fn models_url(provider: &str, api_base: &str) -> String {
    if is_anthropic(provider) {
        return format!("{}/models", crate::llm::provider::anthropic_api_base(api_base));
    }
    let base = api_base.trim_end_matches('/');
    let versioned = base.ends_with("/v1") || base.ends_with("/v1beta");
    if versioned {
        return format!("{base}/models");
    }
    if provider.eq_ignore_ascii_case("ollama") {
        return format!("{base}/v1/models");
    }
    format!("{base}/models")
}

/// Resolve the effective base URL for a provider.
///
/// An explicit `api_base` always wins. When it is empty, the well-known
/// defaults for `openai` / `anthropic` / `ollama` apply — and nothing
/// else: for any other provider the stored bearer key would otherwise be
/// silently sent to `api.openai.com` with zero indication, so the probe
/// fails fast instead of making any request.
///
/// The kind comes from the same [`crate::llm::provider::provider_kind`] the
/// completion path uses, so `"Anthropic"` gets the Anthropic default (and the
/// required-api_base error keeps naming the configured spelling).
pub fn resolve_api_base(cfg: &LLMConfig) -> Result<String> {
    if !cfg.api_base.is_empty() {
        return Ok(cfg.api_base.clone());
    }
    use crate::llm::provider::{provider_kind, ProviderKind};
    match provider_kind(&cfg.provider) {
        ProviderKind::OpenAi => Ok("https://api.openai.com/v1".to_string()),
        ProviderKind::Anthropic => Ok("https://api.anthropic.com".to_string()),
        ProviderKind::OpenAiCompatible if cfg.provider.eq_ignore_ascii_case("ollama") => {
            Ok("http://localhost:11434".to_string())
        }
        ProviderKind::OpenAiCompatible => anyhow::bail!(
            "api_base is required for provider \"{}\" (no well-known default)",
            cfg.provider
        ),
    }
}

/// Probe a provider by asking for its model list ([`models_url`]) with the
/// headers that provider accepts ([`auth_headers`]).
///
/// When `api_base` is empty, falls back to the well-known base URL for
/// `openai` / `anthropic` / `ollama`; any other provider fails fast via
/// [`resolve_api_base`] before any network call is made. Succeeds on any 2xx
/// response. Any failure comes back as a [`ProbeFailure`] carrying its class:
/// a rejected key ([`ProbeFailureKind::Auth`]) reads differently from a
/// provider that never answered ([`ProbeFailureKind::Unreachable`]), which DNS
/// failure, connection refusal and a timeout past [`PROBE_TIMEOUT`] all are.
pub async fn probe_llm_connectivity(cfg: &LLMConfig) -> Result<ProbeOutcome> {
    use reqwest::Client;
    let client = Client::new();

    let base = resolve_api_base(cfg)?;
    let url = models_url(&cfg.provider, &base);

    let mut request = client.get(&url).timeout(PROBE_TIMEOUT);
    for (name, value) in auth_headers(&cfg.provider, &cfg.api_key) {
        request = request.header(name, value);
    }

    let resp = match request.send().await {
        Ok(resp) => resp,
        Err(e) => return Err(anyhow::Error::new(ProbeFailure::unreachable(cfg, url, &e))),
    };

    let status = resp.status();
    if !status.is_success() {
        return Err(anyhow::Error::new(ProbeFailure::from_status(cfg, url, status)));
    }
    Ok(ProbeOutcome { resolved_base: base })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn cfg(provider: &str, api_base: &str, api_key: &str) -> LLMConfig {
        LLMConfig {
            provider: provider.to_string(),
            model: "test-model".to_string(),
            api_key: api_key.to_string(),
            api_base: api_base.to_string(),
            max_tokens: 4096,
            temperature: 0.3,
            disable_thinking: None,
            disabled: false,
        }
    }

    /// Base URL of a port nothing listens on (bound, then released), so the
    /// "unreachable" test never depends on a fixed port being free.
    async fn closed_base() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        format!("http://{addr}")
    }

    // ─── The request shape (RENG-66) ─────────────

    #[test]
    fn models_url_prefixes_v1_only_where_the_api_needs_it() {
        // Configured by host, listing under /v1.
        assert_eq!(
            models_url("anthropic", "https://api.anthropic.com"),
            "https://api.anthropic.com/v1/models"
        );
        assert_eq!(
            models_url("ollama", "http://localhost:11434"),
            "http://localhost:11434/v1/models"
        );
        // A versioned base is used verbatim — never /v1/v1/models.
        assert_eq!(
            models_url("anthropic", "https://api.anthropic.com/v1"),
            "https://api.anthropic.com/v1/models"
        );
        assert_eq!(
            models_url("anthropic", "https://api.anthropic.com/v1/"),
            "https://api.anthropic.com/v1/models"
        );
        // OpenAI-compatible bases keep their own layout: /v1 when they name it,
        // root /models for a host that serves it there (DeepSeek).
        assert_eq!(
            models_url("openai", "https://api.openai.com/v1"),
            "https://api.openai.com/v1/models"
        );
        assert_eq!(
            models_url("deepseek", "https://api.deepseek.com"),
            "https://api.deepseek.com/models"
        );
        assert_eq!(
            models_url("openai", "http://127.0.0.1:1234"),
            "http://127.0.0.1:1234/models"
        );
    }

    #[test]
    fn auth_headers_follow_the_provider_kind() {
        assert_eq!(
            auth_headers("anthropic", "sk-ant-test"),
            vec![
                ("x-api-key", "sk-ant-test".to_string()),
                ("anthropic-version", ANTHROPIC_VERSION.to_string()),
            ]
        );
        // Case-insensitive, like the completion path's routing.
        assert_eq!(auth_headers("Anthropic", "k"), auth_headers("anthropic", "k"));
        assert_eq!(
            auth_headers("openai", "sk-test"),
            vec![("Authorization", "Bearer sk-test".to_string())]
        );
        // Any other name is an OpenAI-compatible endpoint (a Claude proxy
        // included): bearer, exactly like the request that will be completed.
        assert_eq!(
            auth_headers("deepseek", "k"),
            vec![("Authorization", "Bearer k".to_string())]
        );
        assert_eq!(
            auth_headers("ollama", ""),
            vec![("Authorization", "Bearer ".to_string())]
        );
    }

    #[test]
    fn resolve_api_base_keeps_defaults_and_rejects_unknown_providers() {
        assert_eq!(
            resolve_api_base(&cfg("openai", "", "k")).unwrap(),
            "https://api.openai.com/v1"
        );
        assert_eq!(
            resolve_api_base(&cfg("anthropic", "", "k")).unwrap(),
            "https://api.anthropic.com"
        );
        assert_eq!(
            resolve_api_base(&cfg("ollama", "", "k")).unwrap(),
            "http://localhost:11434"
        );
        assert_eq!(
            resolve_api_base(&cfg("deepseek", "https://api.deepseek.com", "k")).unwrap(),
            "https://api.deepseek.com"
        );
        let err = resolve_api_base(&cfg("mimo", "", "k")).unwrap_err();
        assert!(
            err.to_string()
                .contains("api_base is required for provider \"mimo\" (no well-known default)"),
            "got {err}"
        );
    }

    // ─── Live probes against a mock provider (RENG-66) ─────────────

    /// The reported bug, end to end: an Anthropic card whose key is GOOD was
    /// reported as `error`. The mock answers 200 ONLY to a request carrying
    /// both Anthropic headers, so a probe still sending `Authorization: Bearer`
    /// matches no mock (wiremock answers 404) and fails this test.
    #[tokio::test]
    async fn anthropic_probe_sends_anthropic_headers_and_judges_healthy() {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .and(header("x-api-key", "sk-ant-good"))
            .and(header("anthropic-version", ANTHROPIC_VERSION))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": []})))
            .mount(&mock)
            .await;

        let outcome = probe_llm_connectivity(&cfg("anthropic", &mock.uri(), "sk-ant-good"))
            .await
            .expect("an Anthropic provider with a valid key must probe healthy");
        assert_eq!(outcome.resolved_base, mock.uri());

        let requests = mock.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1, "a successful probe makes exactly one request");
        let request = &requests[0];
        assert_eq!(request.url.path(), "/v1/models", "Anthropic lists models under /v1");
        assert_eq!(
            request.headers.get("x-api-key").unwrap().to_str().unwrap(),
            "sk-ant-good"
        );
        assert_eq!(
            request.headers.get("anthropic-version").unwrap().to_str().unwrap(),
            ANTHROPIC_VERSION
        );
        assert!(
            request.headers.get("authorization").is_none(),
            "Anthropic must not receive a bearer header"
        );
    }

    /// The reproduction, kept as a test: the request the probe sent BEFORE this
    /// change — `GET {base}/models` with `Authorization: Bearer <key>` — is
    /// answered with 404 by an Anthropic-shaped endpoint, which is exactly why
    /// a working Anthropic card read as `error` in the health page. It pins the
    /// old shape as broken so it cannot come back unnoticed.
    #[tokio::test]
    async fn the_old_bearer_only_request_shape_is_rejected_by_an_anthropic_endpoint() {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .and(header("x-api-key", "sk-ant-good"))
            .and(header("anthropic-version", ANTHROPIC_VERSION))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": []})))
            .mount(&mock)
            .await;

        // Verbatim what `probe_llm_connectivity` used to issue.
        let old_shape = reqwest::Client::new()
            .get(format!("{}/models", mock.uri()))
            .header("Authorization", "Bearer sk-ant-good")
            .send()
            .await
            .unwrap();
        assert_eq!(
            old_shape.status(),
            reqwest::StatusCode::NOT_FOUND,
            "the pre-RENG-66 request must not reach an Anthropic endpoint"
        );
    }

    /// The other half of the acceptance: a wrong key is still an error. The
    /// probe must not turn `healthy` just because it now sends the right header
    /// — the mock rejects every key but the good one.
    #[tokio::test]
    async fn anthropic_probe_with_a_rejected_key_is_an_auth_error() {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(401).set_body_string(r#"{"error":{"type":"authentication_error"}}"#))
            .mount(&mock)
            .await;

        let err = probe_llm_connectivity(&cfg("anthropic", &mock.uri(), "sk-ant-bad"))
            .await
            .expect_err("a rejected key must not be reported healthy");
        let failure = err
            .downcast_ref::<ProbeFailure>()
            .expect("the probe must classify its failure");
        assert_eq!(failure.kind, ProbeFailureKind::Auth);
        assert_eq!(failure.status, Some(401));
        let message = err.to_string();
        assert!(message.contains("authentication failed"), "got {message}");
        assert!(
            !message.contains("sk-ant-bad"),
            "the health message is user-visible and must not echo the key: {message}"
        );
        assert!(
            !message.contains("unreachable"),
            "a 401 is not an unreachability problem: {message}"
        );
    }

    /// An OpenAI-compatible card still gets a bearer token — the fix must not
    /// have flipped the default for everyone else.
    #[tokio::test]
    async fn openai_compatible_probe_still_sends_a_bearer_token() {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .and(header("Authorization", "Bearer sk-good"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": []})))
            .mount(&mock)
            .await;

        probe_llm_connectivity(&cfg("openai", &format!("{}/v1", mock.uri()), "sk-good"))
            .await
            .expect("a bearer-authenticated provider must probe healthy");

        let requests = mock.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].headers.get("authorization").unwrap().to_str().unwrap(),
            "Bearer sk-good"
        );
        assert!(requests[0].headers.get("x-api-key").is_none());
    }

    /// A provider that never answers is `unreachable` and says so — it is not
    /// a credential problem and must not read like one.
    #[tokio::test]
    async fn a_provider_that_never_answers_is_classified_unreachable() {
        let base = closed_base().await;
        let err = probe_llm_connectivity(&cfg("openai", &base, "sk-test"))
            .await
            .expect_err("nothing is listening on that port");
        let failure = err.downcast_ref::<ProbeFailure>().expect("classified");
        assert_eq!(failure.kind, ProbeFailureKind::Unreachable);
        assert_eq!(failure.status, None);
        assert!(!failure.detail.is_empty(), "the transport cause must be reported");
        let message = err.to_string();
        assert!(message.contains("service unreachable"), "got {message}");
        assert!(message.contains("api_base"), "the fix must be named: {message}");
        assert!(!message.contains("authentication failed"), "got {message}");
    }

    /// 404, 5xx and 429 are distinct from both of the above: the provider
    /// answered, so the URL (404) or the provider's own condition (5xx, 429) is
    /// the problem.
    #[tokio::test]
    async fn answered_errors_are_classified_by_status() {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&mock)
            .await;
        let err = probe_llm_connectivity(&cfg("openai", &format!("{}/v1", mock.uri()), "k"))
            .await
            .expect_err("404 is not healthy");
        let failure = err.downcast_ref::<ProbeFailure>().unwrap();
        assert_eq!(failure.kind, ProbeFailureKind::BadEndpoint);
        assert_eq!(failure.status, Some(404));
        assert!(err.to_string().contains("serves no model list"), "got {err}");

        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&mock)
            .await;
        let err = probe_llm_connectivity(&cfg("openai", &format!("{}/v1", mock.uri()), "k"))
            .await
            .expect_err("503 is not healthy");
        let failure = err.downcast_ref::<ProbeFailure>().unwrap();
        assert_eq!(failure.kind, ProbeFailureKind::ProviderError);
        assert_eq!(failure.status, Some(503));
        let message = err.to_string();
        assert!(message.contains("Not a credential problem"), "got {message}");
    }

    /// P2-1: a 429 is the provider throttling this client — it answered, and
    /// `api_base` is exactly right. Reading it as `BadEndpoint` (the pre-r2
    /// behaviour) told the user to check the one thing that was not wrong.
    #[tokio::test]
    async fn a_rate_limited_provider_is_not_told_to_check_its_api_base() {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(429).set_body_string(r#"{"error":{"type":"rate_limit_error"}}"#))
            .mount(&mock)
            .await;

        let err = probe_llm_connectivity(&cfg("anthropic", &mock.uri(), "sk-ant-good"))
            .await
            .expect_err("429 is not healthy");
        let failure = err.downcast_ref::<ProbeFailure>().expect("classified");
        assert_eq!(failure.kind, ProbeFailureKind::ProviderError);
        assert_eq!(failure.status, Some(429));
        let message = err.to_string();
        assert!(message.contains("rate-limiting"), "got {message}");
        assert!(
            !message.contains("Check api_base") && !message.contains("serves no model list"),
            "a throttle is not an address problem: {message}"
        );
    }

    /// The fail-fast path is unchanged: an unknown provider with no api_base
    /// makes no request at all (and never leaks the key anywhere).
    #[tokio::test]
    async fn unknown_provider_without_api_base_fails_before_any_request() {
        let err = probe_llm_connectivity(&cfg("mimo", "", "sk-secret"))
            .await
            .expect_err("no well-known default for this provider");
        assert!(
            err.to_string().contains("api_base is required for provider \"mimo\""),
            "got {err}"
        );
    }

    /// P2-2: the probe and the completion registry resolve the provider name
    /// with the SAME function, so a mixed-case card is probed the way its
    /// completions are routed (`ProviderRegistry::from_configs` sends
    /// "Anthropic" to the Anthropic implementation since RENG-66 r2) and can no
    /// longer read healthy while completing as someone else.
    #[tokio::test]
    async fn a_mixed_case_anthropic_card_is_probed_as_anthropic() {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .and(header("x-api-key", "sk-ant-good"))
            .and(header("anthropic-version", ANTHROPIC_VERSION))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": []})))
            .mount(&mock)
            .await;

        probe_llm_connectivity(&cfg("Anthropic", &mock.uri(), "sk-ant-good"))
            .await
            .expect("a mixed-case Anthropic card must probe as Anthropic");

        let requests = mock.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].url.path(), "/v1/models");
        assert_eq!(
            requests[0].headers.get("x-api-key").unwrap().to_str().unwrap(),
            "sk-ant-good"
        );
        assert!(requests[0].headers.get("authorization").is_none());

        // …and the kind the probe used is the kind the registry will use.
        assert_eq!(
            crate::llm::provider::provider_kind("Anthropic"),
            crate::llm::provider::ProviderKind::Anthropic
        );
    }
}
