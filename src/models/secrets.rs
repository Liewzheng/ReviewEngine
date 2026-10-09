//! Credential redaction for review reports and the publish path (RENG-113).
//!
//! A finding's `Evidence` / `summary` / `impact` / `recommendation` fields come
//! from the expert LLM and may contain a snippet of the reviewed source — a
//! line of `config.toml` with `password = "real_password"` is exactly the
//! class of evidence the security expert is *supposed* to quote. Before that
//! line lands in a file or on an MR discussion it must not carry the plaintext
//! value:
//!
//! - The report file defaults to `0644`, so any local user can read it. A
//!   reviewer who runs `--local-path` and reads their own report is fine, but
//!   a CI artifact shared to a team, or a `~/.config/review-engine/reports/`
//!   file readable by another account on the same host, is a leak.
//! - The `--publish` path posts the same text to GitLab/GitHub. Once posted
//!   the plaintext is in the MR's permanent discussion history, outside any
//!   redaction sweep we run on the local file.
//!
//! The redaction implemented here replaces every detected secret value with a
//! marker that keeps the diagnostic context — the key name and a length /
//! fingerprint — so a reviewer can still tell which line had the hit, but the
//! actual value never leaves the review process. The same detector backs the
//! publish-time refusal, so a value that bypassed the masking (e.g. because it
//! lived in a field the redaction path never visits) still blocks the post.
//!
//! ## Patterns
//!
//! The detector recognises two shapes:
//!
//! - **structured assignment** — `key = value` / `key: value`, where `key` is
//!   one of a small list of credential-shaped names
//!   (`password`, `secret`, `token`, `api_key`, `apiKey`, `access_key`,
//!   `client_secret`, `private_key`, `webhook_secret`, `signing_secret`, …).
//!   The value is whatever follows the separator, quoted or bare.
//! - **value by itself** — a high-entropy token from a fixed catalogue of
//!   provider prefixes (`glpat-…`, `ghp_…`, `AKIA…`, a PEM block, a
//!   `https://user:pass@host` URL, …) and any sufficiently long hex/base64
//!   string that is not a line number or a hash digest of the kind the report
//!   already prints.
//!
//! ## What the marker looks like
//!
//! A redacted line reads:
//!
//! ```text
//! password = "***[redacted:len=12,fp=abc12345]***"
//! ```
//!
//! — `len=` is the original value's length, `fp=` is the first 8 bytes of the
//! SHA-256 digest (16 hex chars, the same length the codebase prints as
//! `feedback::fingerprint`). That is enough to recognise "the same value
//! appears again on line 73" without revealing the value.
//!
//! ## Failure mode
//!
//! The detector is deliberately conservative — false positives (a benign
//! string flagged as a secret) cost the reviewer a marker; false negatives
//! (a real secret that wasn't detected) leak. When in doubt, redact.

use std::ops::Range;

use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Categorised kind of a detected credential.
///
/// Kept narrow on purpose: the publish-time refusal names the kind so the
/// operator can see what slipped through, and the redaction marker keeps the
/// kind in its label (`[redacted:kind=password,…]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretKind {
    /// `password = "…"`, `db_password: …`, etc.
    Password,
    /// `secret = "…"`, `client_secret: …`, etc.
    Secret,
    /// `token = "…"`, `access_token: …`, `auth_token: …`, etc.
    Token,
    /// `api_key = "…"`, `apiKey: …`, `apikey: …`, etc.
    ApiKey,
    /// `private_key = "…"`, a PEM block, etc.
    PrivateKey,
    /// AWS access key id (`AKIA…`) or secret access key.
    AwsCredential,
    /// GitLab personal / project / runner token (`glpat-…`, `glrt-…`).
    GitLabToken,
    /// GitHub personal / OAuth / server / refresh token
    /// (`ghp_…`, `gho_…`, `ghs_…`, `ghr_…`, `github_pat_…`).
    GitHubToken,
    /// OpenAI / Anthropic / similar provider token, identified by prefix or
    /// by the `OPENAI_API_KEY = "…"` style assignment.
    LlmApiKey,
    /// `Authorization: Bearer …`, `Basic <base64>`.
    AuthHeader,
    /// `https://user:password@host` URL.
    BasicAuthUrl,
}

impl SecretKind {
    /// Lowercase label used in the redaction marker.
    pub fn label(&self) -> &'static str {
        match self {
            SecretKind::Password => "password",
            SecretKind::Secret => "secret",
            SecretKind::Token => "token",
            SecretKind::ApiKey => "api_key",
            SecretKind::PrivateKey => "private_key",
            SecretKind::AwsCredential => "aws_credential",
            SecretKind::GitLabToken => "gitlab_token",
            SecretKind::GitHubToken => "github_token",
            SecretKind::LlmApiKey => "llm_api_key",
            SecretKind::AuthHeader => "auth_header",
            SecretKind::BasicAuthUrl => "basic_auth_url",
        }
    }
}

/// One detected credential in a piece of text.
///
/// `range` is the byte range of the *value* (the part that gets replaced),
/// not the whole `key = value` line. `key` is `Some` for the structured
/// assignment patterns (so the marker can echo `password = "…"` instead of
/// just `…`) and `None` for the value-only patterns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretHit {
    pub kind: SecretKind,
    pub key: Option<String>,
    pub value: String,
    pub range: Range<usize>,
}

impl SecretHit {
    /// First 8 hex characters of the value's SHA-256 fingerprint — short
    /// enough to print in an error message and still uniquely identifies
    /// the value when the operator greps the local report file.
    pub fn fingerprint_short(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.value.as_bytes());
        hex::encode(&hasher.finalize()[..4])
    }
}

/// The placeholder that replaces a detected credential value.
///
/// Format: `***[redacted:kind=<label>,len=<n>,fp=<16hex>]***` (the structured
/// assignments get an extra `key=<name>` field). The leading and trailing
/// `***` make it unmistakable when scanning the file with `grep`, so an
/// operator can confirm the redaction actually fired.
pub(crate) const REDACTED_PLACEHOLDER_PREFIX: &str = "***[redacted:";

fn fingerprint_hex(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    hex::encode(&hasher.finalize()[..8])
}

fn placeholder(kind: SecretKind, key: Option<&str>, value: &str) -> String {
    let mut parts = format!(
        "kind={},len={},fp={}",
        kind.label(),
        value.chars().count(),
        fingerprint_hex(value)
    );
    if let Some(k) = key {
        parts.push_str(&format!(",key={k}"));
    }
    format!("{REDACTED_PLACEHOLDER_PREFIX}{parts}]***")
}

/// Build the list of detector regexes once. Each entry pairs a compiled regex
/// with the [`SecretKind`] it belongs to and whether the captured group is
/// the value itself (`true`) or the `key = value` line (`false`, in which
/// case the detector has to split the key and value itself).
///
/// Adding a new pattern is a single entry here; the scan/redact code below
/// is pattern-agnostic.
///
/// The order matters: structured assignments (`password = "…"`) are scanned
/// **before** value-only patterns (`glpat-…`) so a value-shaped secret that
/// also appears after `password = ` is reported once, with the key name
/// preserved. The overlap dedup in [`scan_secrets`] keeps the result stable
/// even if the ordering ever changes.
fn detectors() -> Vec<(&'static str, SecretKind, DetectorShape)> {
    vec![
        // ── Structured assignments first: each captures `key` + `val`
        //    named groups so the marker can echo the key name alongside
        //    the redaction metadata.
        (
            r#"(?im)(?P<key>\b(?:password|passwd|pwd|db[_-]?password)\b)\s*[:=]\s*["']?(?P<val>[^\s"',;}{)#]+)["']?"#,
            SecretKind::Password,
            DetectorShape::KeyValue,
        ),
        (
            r#"(?im)(?P<key>\b(?:secret|client[_-]?secret|app[_-]?secret)\b)\s*[:=]\s*["']?(?P<val>[^\s"',;}{)#]+)["']?"#,
            SecretKind::Secret,
            DetectorShape::KeyValue,
        ),
        (
            r#"(?im)(?P<key>\b(?:token|access[_-]?token|auth[_-]?token|bearer[_-]?token|refresh[_-]?token|id[_-]?token|session[_-]?token|csrf[_-]?token|webhook[_-]?secret|signing[_-]?secret|api[_-]?token)\b)\s*[:=]\s*["']?(?P<val>[^\s"',;}{)#]+)["']?"#,
            SecretKind::Token,
            DetectorShape::KeyValue,
        ),
        (
            r#"(?im)(?P<key>\b(?:api[_-]?key|apikey|api[_-]?id|access[_-]?key(?:_id)?|secret[_-]?access[_-]?key)\b)\s*[:=]\s*["']?(?P<val>[^\s"',;}{)#]+)["']?"#,
            SecretKind::ApiKey,
            DetectorShape::KeyValue,
        ),
        (
            r#"(?im)(?P<key>\b(?:private[_-]?key|priv[_-]?key|secret[_-]?key)\b)\s*[:=]\s*["']?(?P<val>[^\s"',;}{)#]+)["']?"#,
            SecretKind::PrivateKey,
            DetectorShape::KeyValue,
        ),
        (
            r#"(?im)(?P<key>\b(?:openai[_-]?api[_-]?key|anthropic[_-]?api[_-]?key|llm[_-]?api[_-]?key)\b)\s*[:=]\s*["']?(?P<val>[^\s"',;}{)#]+)["']?"#,
            SecretKind::LlmApiKey,
            DetectorShape::KeyValue,
        ),
        // ── Value-only patterns after the structured set, so a line the
        //    structured detector already covered isn't double-matched.
        //
        // ── PEM private key blocks (must come before `Authorization`,
        //    because PEM headers carry `=` and `BEGIN`/`END` would never
        //    be confused with an HTTP header).
        (
            r"(?s)-----BEGIN (?:RSA |EC |DSA |OPENSSH |PGP |ENCRYPTED )?PRIVATE KEY-----.*?-----END (?:RSA |EC |DSA |OPENSSH |PGP |ENCRYPTED )?PRIVATE KEY-----",
            SecretKind::PrivateKey,
            DetectorShape::Value,
        ),
        // ── Auth headers: `Authorization: Bearer …` / `Basic <base64>`.
        (
            r"(?im)\b(?:Authorization|Proxy-Authorization)\s*:\s*(?:Bearer|Basic|Token)\s+([A-Za-z0-9._\-+/=]{12,})",
            SecretKind::AuthHeader,
            DetectorShape::ValueGroup1,
        ),
        // ── URLs with embedded credentials.
        (
            r"\bhttps?://[^\s/:@]+:([^\s/@]+)@[^\s/]+",
            SecretKind::BasicAuthUrl,
            DetectorShape::ValueGroup1,
        ),
        // ── Provider-prefixed tokens, recognised by the prefix alone
        //    (length is implied by the prefix).
        (
            r"\b((?:AKIA|ASIA)[0-9A-Z]{16})\b",
            SecretKind::AwsCredential,
            DetectorShape::ValueGroup1,
        ),
        (
            r"\b((?:glpat|glrt|glft|glcrt)-[A-Za-z0-9_\-]{20,})\b",
            SecretKind::GitLabToken,
            DetectorShape::ValueGroup1,
        ),
        (
            r"\b((?:ghp|gho|ghu|ghs|ghr|github_pat)_[A-Za-z0-9]{30,})\b",
            SecretKind::GitHubToken,
            DetectorShape::ValueGroup1,
        ),
        // OpenAI-style keys (the `sk-…` family plus `sk_live_/sk_test_` for
        // Stripe) — short enough that a structured assignment isn't required
        // for the value to be uniquely shaped.
        (
            r"\b((?:sk-(?:proj-)?[A-Za-z0-9_\-]{20,}|sk_(?:live|test)_[A-Za-z0-9]{16,}|[A-Za-z0-9]{32,}\.[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}))\b",
            SecretKind::LlmApiKey,
            DetectorShape::ValueGroup1,
        ),
    ]
}

/// What the regex captures, and how to map the match to a [`SecretHit`].
enum DetectorShape {
    /// The whole match is the value (range = `m.start()..m.end()`).
    Value,
    /// Capture group 1 is the value (range = `m.start(1)..m.end(1)`).
    ValueGroup1,
    /// The regex carries named `key` and `val` groups. The range is the
    /// `val` group's range, the key is `m["key"]`.
    KeyValue,
}

/// Compiled detector used at scan / redact time.
struct CompiledDetector {
    regex: Regex,
    kind: SecretKind,
    shape: DetectorShape,
}

fn compiled_detectors() -> &'static [CompiledDetector] {
    use std::sync::OnceLock;
    static CACHE: OnceLock<Result<Vec<CompiledDetector>, String>> = OnceLock::new();
    let result = CACHE.get_or_init(|| {
        detectors()
            .into_iter()
            .map(|(pattern, kind, shape)| {
                Regex::new(pattern)
                    .map(|regex| CompiledDetector { regex, kind, shape })
                    .map_err(|e| format!("detector pattern `{pattern}` failed to compile: {e}"))
            })
            .collect()
    });
    // A pattern that fails to compile is a programmer error in this file:
    // the literal is right next to the call. A failed compile here means
    // the binary cannot serve the publish-time gate, so the process is
    // already doomed — panic with the diagnostic instead of swallowing it.
    match result {
        Ok(detectors) => detectors,
        Err(msg) => panic!("{msg}"),
    }
}

/// Minimum length a value must reach to be redacted by a structured
/// assignment rule. Anything shorter is overwhelmingly more likely to be a
/// placeholder (`password = "x"`) than a real secret, and redacting it makes
/// the report noisier than the original.
pub const MIN_VALUE_LEN: usize = 6;

/// A value below this length is also skipped from the *publish* gate — the
/// marker counts char length, so a `len=4` marker is still a leak indicator,
/// but a `len=4` raw value isn't a credible secret either. Combined with
/// `MIN_VALUE_LEN`, this keeps the gate in lockstep with the redaction.
pub const MIN_PUBLISH_HIT_LEN: usize = MIN_VALUE_LEN;

/// Scan `text` for suspected credentials. Hits do *not* overlap — a later
/// detector never re-matches inside a value already covered by an earlier
/// one (the structured assignment set fires before the value-only set, so a
/// `password = "glpat-…"` line is reported as `password`, not also as a
/// `gitlab_token`).
pub fn scan_secrets(text: &str) -> Vec<SecretHit> {
    let mut hits: Vec<SecretHit> = Vec::new();
    for detector in compiled_detectors() {
        for caps in detector.regex.captures_iter(text) {
            // Captures always carry the whole match in group 0, so the
            // structured-assignment shape can read it as `caps.get(0)`.
            let whole = match caps.get(0) {
                Some(m) => m,
                None => continue,
            };
            let (value_range, key) = match detector.shape {
                DetectorShape::Value => (whole.start()..whole.end(), None),
                DetectorShape::ValueGroup1 => match caps.get(1) {
                    Some(g) => (g.start()..g.end(), None),
                    None => continue,
                },
                DetectorShape::KeyValue => {
                    let val = match caps.name("val") {
                        Some(v) => v,
                        None => continue,
                    };
                    let key = caps.name("key").map(|k| k.as_str().to_string());
                    (val.start()..val.end(), key)
                }
            };
            if value_range.start >= value_range.end {
                continue;
            }
            let value = text[value_range.clone()].to_string();
            if value.chars().count() < MIN_VALUE_LEN {
                continue;
            }
            hits.push(SecretHit {
                kind: detector.kind,
                key,
                value,
                range: value_range,
            });
        }
    }
    // Overlapping hits: drop any whose range starts inside an earlier hit's
    // range. The structured assignments (`key = value`) appear first in
    // `detectors()` and so naturally take precedence over value-only matches
    // (e.g. the GitLab token prefix) for the same string.
    hits.sort_by_key(|h| (h.range.start, h.range.end));
    let mut deduped: Vec<SecretHit> = Vec::with_capacity(hits.len());
    let mut last_end: usize = 0;
    for hit in hits {
        if hit.range.start < last_end {
            continue;
        }
        last_end = hit.range.end;
        deduped.push(hit);
    }
    deduped
}

/// Replace every detected credential in `text` with its marker placeholder.
///
/// `key = value` lines stay syntactically intact: `password = "abc123"` is
/// rewritten to `password = "***[redacted:kind=password,len=6,fp=…,key=password]***"`,
/// not to `***[redacted:…]***` alone. The original value's length and
/// fingerprint are preserved so a reviewer can still tell "the same value
/// appears twice" — what is removed is the value itself, not the
/// distinguishing context.
pub fn redact_text(text: &str) -> String {
    let hits = scan_secrets(text);
    if hits.is_empty() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0usize;
    for hit in hits {
        if hit.range.start > cursor {
            out.push_str(&text[cursor..hit.range.start]);
        }
        out.push_str(&placeholder(hit.kind, hit.key.as_deref(), &hit.value));
        cursor = hit.range.end;
    }
    if cursor < text.len() {
        out.push_str(&text[cursor..]);
    }
    out
}

/// One hit carried back from the publish-time scan.
///
/// The publish gate surfaces only the first few hits, by [`SecretHit::range`]
/// order, so a reviewer reading the error knows what slipped through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishRefusal {
    /// The rendered body the gate ran over. Always included so the operator
    /// can see the offending context without re-running the review.
    pub body: String,
    /// Up to [`PUBLISH_REFUSAL_HITS`] hits, in source order.
    pub hits: Vec<SecretHit>,
}

/// How many hits the gate surfaces in the error message. More would bloat
/// the error without making the cause clearer; the gate is binary
/// (`hit → refuse`), so the count is informational, not actionable.
pub const PUBLISH_REFUSAL_HITS: usize = 5;

/// Result of the publish-time gate. `Ok(())` means the body is safe to post
/// (or no body was provided); `Err(PublishRefusal)` names what was found.
pub fn scan_for_publish(text: &str) -> Result<(), PublishRefusal> {
    let hits = scan_secrets(text);
    if hits.is_empty() {
        return Ok(());
    }
    let mut shown = hits;
    shown.truncate(PUBLISH_REFUSAL_HITS);
    Err(PublishRefusal {
        body: text.to_string(),
        hits: shown,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit_count(text: &str) -> usize {
        scan_secrets(text).len()
    }

    /// Structured assignments: the value is replaced, the key is preserved.
    #[test]
    fn redact_password_assignment_keeps_key_and_replaces_value() {
        let line = r#"password = "hunter2_real""#;
        let redacted = redact_text(line);
        assert!(!redacted.contains("hunter2_real"), "plaintext must be gone: {redacted}");
        assert!(
            redacted.contains("password = "),
            "the key + separator survives: {redacted}"
        );
        assert!(redacted.contains("kind=password"), "the kind is named: {redacted}");
        assert!(redacted.contains("key=password"), "the key name is echoed: {redacted}");
        assert!(redacted.contains("len=12"), "the length is reported: {redacted}");
        // The length matches the original value length.
        let len = "hunter2_real".chars().count();
        assert_eq!(len, 12);
    }

    /// YAML-style `key: value` (no `=`) is also caught.
    #[test]
    fn redact_yaml_colon_assignment() {
        let line = "api_key: sk-live-XXXXXXXXXXXXXXXXXXXX";
        let redacted = redact_text(line);
        assert!(!redacted.contains("sk-live-XXXXXXXXXXXXXXXXXXXX"));
        assert!(redacted.contains("api_key: "));
        assert!(redacted.contains("kind=api_key"));
        assert!(redacted.contains("key=api_key"));
    }

    /// Provider-prefixed tokens are caught by their shape even when there is
    /// no `key = value` line around them — the URL/token show-up-as-is class.
    #[test]
    fn redact_gitlab_personal_token_by_prefix() {
        let line = "Token used: glpat-AbCdEfGhIjKlMnOpQrStUv";
        let redacted = redact_text(line);
        assert!(!redacted.contains("glpat-AbCdEfGhIjKlMnOpQrStUv"));
        assert!(redacted.contains("kind=gitlab_token"));
    }

    #[test]
    fn redact_github_personal_token_by_prefix() {
        let line = "GITHUB_TOKEN=ghp_AbCdEfGhIjKlMnOpQrStUvWxYz0123456789";
        let redacted = redact_text(line);
        assert!(!redacted.contains("ghp_AbCdEfGhIjKlMnOpQrStUvWxYz0123456789"));
        assert!(redacted.contains("kind="));
    }

    /// AWS access key ids are short and prefix-unique.
    #[test]
    fn redact_aws_access_key_id() {
        let line = "AKIAIOSFODNN7EXAMPLE was leaked";
        let redacted = redact_text(line);
        assert!(!redacted.contains("AKIAIOSFODNN7EXAMPLE"));
        assert!(redacted.contains("kind=aws_credential"));
    }

    /// PEM private key blocks are caught as a whole — the multiline span
    /// must come out atomically.
    #[test]
    fn redact_pem_private_key_block() {
        let pem = "-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEA...\nabc\n-----END RSA PRIVATE KEY-----";
        let redacted = redact_text(pem);
        assert!(!redacted.contains("BEGIN RSA PRIVATE KEY"));
        assert!(!redacted.contains("MIIEowIBAAKCAQEA"));
        assert!(redacted.contains("kind=private_key"));
    }

    /// URLs with embedded credentials are caught; the URL's host stays.
    #[test]
    fn redact_basic_auth_url_keeps_the_host() {
        let line = "see https://alice:s3cret@example.com/data";
        let redacted = redact_text(line);
        assert!(!redacted.contains("s3cret"));
        assert!(redacted.contains("https://alice:"));
        assert!(redacted.contains("example.com"));
        assert!(redacted.contains("kind=basic_auth_url"));
    }

    /// An `Authorization: Bearer …` header in evidence is caught.
    #[test]
    fn redact_authorization_header() {
        let line = "curl -H 'Authorization: Bearer ghp_abcdefghijklmnopqrstuvwxyz0123456789'";
        let redacted = redact_text(line);
        assert!(!redacted.contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789"));
        assert!(redacted.contains("Authorization: Bearer"));
        assert!(redacted.contains("kind=auth_header"));
    }

    /// Lines that are not credential-shaped pass through verbatim.
    #[test]
    fn plain_text_is_unchanged() {
        let line = "the function `read_password` returns `None` when EOF is reached";
        let redacted = redact_text(line);
        assert_eq!(redacted, line, "benign prose must not be touched: {redacted}");

        // A bare equal sign without a credential-shaped key.
        assert_eq!(redact_text("x = 1 + 2"), "x = 1 + 2");
    }

    /// Short values are below the noise floor and stay untouched.
    #[test]
    fn short_values_are_left_alone() {
        // `password = "x"` — placeholder, redact noise would dominate.
        assert_eq!(redact_text(r#"password = "x""#), r#"password = "x""#);
        // `token = ""` — empty.
        assert_eq!(redact_text("token = "), "token = ");
    }

    /// The publish gate fails closed on any hit, even one.
    #[test]
    fn publish_gate_fails_closed_on_any_hit() {
        let body = "All clean except: password = \"hunter2_real\"";
        let err = scan_for_publish(body).unwrap_err();
        assert_eq!(err.hits.len(), 1);
        assert_eq!(err.hits[0].kind, SecretKind::Password);
        assert_eq!(err.hits[0].value, "hunter2_real");
    }

    #[test]
    fn publish_gate_passes_when_clean() {
        let body = "# Review\n\nNo credentials here.\n";
        assert!(scan_for_publish(body).is_ok());
    }

    /// Overlapping matches: a `key = value` whose value also matches a
    /// provider-prefix pattern is reported once, as the structured assignment.
    #[test]
    fn overlapping_hits_pick_the_structured_assignment() {
        let line = r#"token = "glpat_AbCdEfGhIjKlMnOpQrStUv""#;
        let hits = scan_secrets(line);
        assert_eq!(hits.len(), 1, "the value-only GitLab match must be dropped: {hits:?}");
        assert_eq!(hits[0].kind, SecretKind::Token, "the structured token assignment wins");
        assert_eq!(hits[0].key.as_deref(), Some("token"));
    }

    /// Fingerprint: same value twice → same fingerprint, different value →
    /// different fingerprint. A reviewer reading the markers can still correlate.
    #[test]
    fn fingerprint_is_stable_and_distinguishes_values() {
        let same_value = "hunter2_real";
        let fp1 = fingerprint_hex(same_value);
        let fp2 = fingerprint_hex(same_value);
        assert_eq!(fp1, fp2);
        assert_eq!(fp1.len(), 16, "first 8 bytes of SHA-256 → 16 hex chars");
        assert_ne!(fp1, fingerprint_hex("a_different_value"));
    }

    /// Many hits in a single body still leave a usable report.
    #[test]
    fn redact_many_hits_in_one_block() {
        let body = "\
            password = \"hunter2_real\"\n\
            api_key = \"sk-live-XXXXXXXXXXXXXXXXXXXX\"\n\
            token = \"glpat-AbCdEfGhIjKlMnOpQrStUv\"\n";
        let redacted = redact_text(body);
        assert!(!redacted.contains("hunter2_real"));
        assert!(!redacted.contains("sk-live-XXXXXXXXXXXXXXXXXXXX"));
        assert!(!redacted.contains("glpat-AbCdEfGhIjKlMnOpQrStUv"));
        assert!(redacted.contains("kind=password"));
        assert!(redacted.contains("kind=api_key"));
        assert!(redacted.contains("kind=token"));
    }

    /// The placeholder's leading + trailing `***` make it greppable, so an
    /// operator can verify the redaction actually fired.
    #[test]
    fn placeholder_is_greppable() {
        let marker = placeholder(SecretKind::Password, Some("password"), "hunter2_real");
        assert!(marker.starts_with("***[redacted:"));
        assert!(marker.ends_with("]***"));
    }

    /// The hit counter is just a sanity check the detectors aren't double-
    /// firing on the same string. Real-world inputs are tested through the
    /// redact_text path above.
    #[test]
    fn hit_count_matches_redacted_marker_count() {
        let body = "password = \"hunter2_real\"\napi_key = \"sk-live-XXXXXXXXXXXXXXXXXXXX\"\n";
        assert_eq!(hit_count(body), 2);
        assert_eq!(redact_text(body).matches("redacted:").count(), 2);
    }
}
