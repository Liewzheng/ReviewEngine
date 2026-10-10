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
//!   `https://user:pass@host` URL, an `Authorization: Bearer …` header, …).
//!   There is **no** generic hex/base64 entropy detector — false positives
//!   would dominate (every long SHA / UUID is. RFC 4648). The catalogue is
//!   the surface; what isn't named isn't masked.
//!
//! ## What the marker looks like
//!
//! A redacted line reads:
//!
//! ```text
//! password = "***[redacted:kind=password,len=12,fp=abcdef0123456789,key=password]***"
//! ```
//!
//! — `kind=` is the [`SecretKind`] label, `len=` is the original value's
//! length (in chars), `fp=` is the **first 8 bytes** of the SHA-256 digest
//! of the value (**16 hex chars**, the same length the codebase prints as
//! `feedback::fingerprint`). For value-only patterns where no key name
//! exists, the `key=` field is omitted. That is enough to recognise "the
//! same value appears again on line 73" without revealing the value, and
//! it lets [`SecretHit::fingerprint_short`] print an 8-hex prefix in the
//! publish-refusal error message (the operator greps the local report file
//! for the same 8-hex prefix to find the offending line).
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
///
/// `Debug` is implemented by hand (not derived) — a derived `Debug` would
/// print the `value` field verbatim, and the module's docstring promises the
/// opposite: a hit's plaintext value never leaves the redact / scan code
/// path via `{:?}`. See [`SecretHit::fingerprint_short`] for the
/// fingerprint the manual `Debug` surfaces as the operator's join key.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretHit {
    pub kind: SecretKind,
    pub key: Option<String>,
    pub value: String,
    pub range: Range<usize>,
}

impl std::fmt::Debug for SecretHit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Deliberately do NOT include `self.value` — the plaintext would
        // land in any logger / error formatter that handles `SecretHit` by
        // its `Debug` impl (tracing, anyhow, panic payload, etc.). The
        // operator-visible context (kind, key, length, fingerprint prefix)
        // is enough to grep the local report file for the offending line.
        f.debug_struct("SecretHit")
            .field("kind", &self.kind)
            .field("key", &self.key)
            .field("len", &self.value.chars().count())
            .field("fingerprint", &self.fingerprint_short())
            .field("range", &self.range)
            .finish()
    }
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
        //
        //    The optional `["']?` around the key lets the same regex
        //    cover JSON-style `"password": "…"`, TOML/YAML-style
        //    `password = "…"` and Python-dict-style `'password': "…"`
        //    with one pattern — the lead `["']?` matches the JSON opening
        //    quote (if any), the trailing `["']?` matches the closing
        //    one (if any), and the inner `\s*[:=]\s*` consumes the
        //    surrounding whitespace + separator. Without it the JSON
        //    form slips the gate (the `\"` quoting for the key is one of
        //    W14's documented bypass shapes).
        (
            r#"(?im)["']?(?P<key>\b(?:password|passwd|pwd|db[_-]?password)\b)["']?\s*[:=]\s*["']?(?P<val>[^\s"',;}{)#]+)["']?"#,
            SecretKind::Password,
            DetectorShape::KeyValue,
        ),
        (
            r#"(?im)["']?(?P<key>\b(?:secret|client[_-]?secret|app[_-]?secret)\b)["']?\s*[:=]\s*["']?(?P<val>[^\s"',;}{)#]+)["']?"#,
            SecretKind::Secret,
            DetectorShape::KeyValue,
        ),
        (
            r#"(?im)["']?(?P<key>\b(?:token|access[_-]?token|auth[_-]?token|bearer[_-]?token|refresh[_-]?token|id[_-]?token|session[_-]?token|csrf[_-]?token|webhook[_-]?secret|signing[_-]?secret|api[_-]?token)\b)["']?\s*[:=]\s*["']?(?P<val>[^\s"',;}{)#]+)["']?"#,
            SecretKind::Token,
            DetectorShape::KeyValue,
        ),
        (
            r#"(?im)["']?(?P<key>\b(?:api[_-]?key|apikey|api[_-]?id|access[_-]?key(?:_id)?|secret[_-]?access[_-]?key)\b)["']?\s*[:=]\s*["']?(?P<val>[^\s"',;}{)#]+)["']?"#,
            SecretKind::ApiKey,
            DetectorShape::KeyValue,
        ),
        (
            r#"(?im)["']?(?P<key>\b(?:private[_-]?key|priv[_-]?key|secret[_-]?key)\b)["']?\s*[:=]\s*["']?(?P<val>[^\s"',;}{)#]+)["']?"#,
            SecretKind::PrivateKey,
            DetectorShape::KeyValue,
        ),
        (
            r#"(?im)["']?(?P<key>\b(?:openai[_-]?api[_-]?key|anthropic[_-]?api[_-]?key|llm[_-]?api[_-]?key)\b)["']?\s*[:=]\s*["']?(?P<val>[^\s"',;}{)#]+)["']?"#,
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

/// Regex matching the marker the redact pass itself emits: `***[redacted:…]***`.
///
/// Compiled once. Used both to make the scan marker-aware (so a board
/// carrying markers doesn't trip the publish gate — see [`scan_secrets`])
/// and to drive the redact pass's idempotence: `redact_text(x) == redact_text(redact_text(x))`,
/// because every marker span is treated as already redacted.
fn marker_regex() -> &'static Regex {
    use std::sync::OnceLock;
    static CACHE: OnceLock<Result<Regex, String>> = OnceLock::new();
    let result = CACHE.get_or_init(|| {
        // The marker's body is comma-separated `key=value` pairs. The
        // delimiter is `]` (right square bracket); a future field that
        // happened to contain one would break this — kept on the
        // conservative side, since `placeholder()` only writes the
        // field names documented at [`SecretKind::label`] / `key`.
        Regex::new(r"\*\*\*\[redacted:[^]]*\]\*\*\*").map_err(|e| format!("marker regex failed to compile: {e}"))
    });
    // A pattern that fails to compile is a programmer error in this file:
    // the literal is right next to the call. A failed compile here means
    // the binary cannot serve the publish-time gate, so the process is
    // already doomed — panic with the diagnostic instead of swallowing it.
    match result {
        Ok(rx) => rx,
        Err(msg) => panic!("{msg}"),
    }
}

/// Find every byte range of `text` covered by a redaction marker.
///
/// Marker spans are sorted by start and do not overlap (a redaction marker
/// cannot contain another). Empty input yields no spans.
fn marker_spans(text: &str) -> Vec<Range<usize>> {
    marker_regex().find_iter(text).map(|m| m.start()..m.end()).collect()
}

/// Unescape JSON-style `\"` → `"` in `text`, returning both the
/// normalized string and a side-product byte-offset map from each byte
/// of the normalized string back to where it came from in the original.
///
/// W14 (RENG secrets scan / redaction pass): raw LLM output may carry a
/// JSON payload with `\"` quoting the keys / values, e.g.
///
/// ```text
/// {\"password\": \"hunter2_real\"}
/// ```
///
/// The structured detector's value char class excludes `"`, so without
/// pre-normalization the regex starts capturing at the `\` (a single
/// backslash — below `MIN_VALUE_LEN`) and the key/value token fall
/// through the gate. The value-only patterns (`glpat-…`, `AKIA…`, …) are
/// not affected (they don't quote); this normalizer lifts the
/// structured-detector bypass without relaxing any other rule.
///
/// The map has length `normalized.len() + 1`: entry `j` for `j` in
/// `0..normalized.len()` is the byte offset in `text` that produced the
/// `j`-th byte of the normalized string, and the trailing entry is
/// `text.len()` (used as the "end of input" sentinel for range mapping).
fn unescape_quotes_for_scan(text: &str) -> (String, Vec<usize>) {
    let mut normalized = String::with_capacity(text.len());
    let mut map: Vec<usize> = Vec::with_capacity(text.len() + 1);
    let mut chars = text.char_indices().peekable();
    while let Some((byte_pos, ch)) = chars.next() {
        if ch == '\\' && matches!(chars.peek(), Some(&(_, '"'))) {
            // \" → " (W14): two original bytes collapse to one
            // normalized byte, mapped back to the leading `\` position.
            chars.next();
            normalized.push('"');
            map.push(byte_pos);
            continue;
        }
        let ch_len = ch.len_utf8();
        for _ in 0..ch_len {
            map.push(byte_pos);
        }
        normalized.push(ch);
    }
    map.push(text.len());
    (normalized, map)
}

/// Scan `text` for suspected credentials. Hits do *not* overlap — a later
/// detector never re-matches inside a value already covered by an earlier
/// one (the structured assignment set fires before the value-only set, so a
/// `password = "glpat-…"` line is reported as `password`, not also as a
/// `gitlab_token`).
///
/// RENG-113 round 2: any hit whose range overlaps an existing redaction
/// marker is dropped. The marker's `***[redacted:…]***` is structurally a
/// `key = value` line (`kind=<x>` is a key, the comma-joined fields are a
/// value class `[^\s,]+`) and the basic-auth URL pattern matches the
/// `user:***[…]***@host` shape. Without this guard a redacted body would
/// re-fire the gate on its own output (and a non-credential text would too
/// — `api_key = process.env.API_KEY` redacts to a marker, the marker
/// re-matches the password detector, and the publish is refused with an
/// unactionable "re-redact locally" message). Treating marker spans as
/// "already redacted, do not look inside" keeps the gate, the renderer
/// and the redact pass composable.
///
/// W14 (RENG secrets scan / redaction pass): `text` is pre-normalized
/// with `unescape_quotes_for_scan` so JSON-quoted payloads
/// (`{\"password\":\"hunter2\"}`) hit the structured detector. All
/// returned ranges are mapped back to the *original* text — callers
/// slicing with `&original[hit.range]` see exactly the bytes that
/// produced the hit.
pub fn scan_secrets(text: &str) -> Vec<SecretHit> {
    let (normalized, pos_map) = unescape_quotes_for_scan(text);
    // Marker spans first — cheap (a single regex pass) and load-bearing.
    let markers = marker_spans(&normalized);
    let mut hits: Vec<SecretHit> = Vec::new();
    for detector in compiled_detectors() {
        for caps in detector.regex.captures_iter(&normalized) {
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
            // RENG-113 round 2: drop any hit whose value range starts
            // inside a marker span. The marker is, by construction, the
            // replacement for an earlier hit — re-scanning it would
            // either re-fire the same detector or, worse, match a
            // different one (the basic-auth URL pattern catches the
            // `user:***[…]***@host` shape).
            if markers
                .iter()
                .any(|m| value_range.start >= m.start && value_range.start < m.end)
            {
                continue;
            }
            // The value is sliced out of the *normalized* form, but the
            // value text itself is unchanged by the `\"` → `"` pass (only
            // the surrounding quotes shift), so the string is the same
            // content as the original slice.
            let value = normalized[value_range.clone()].to_string();
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
    // Map every surviving hit's normalized-byte range back into
    // original-text coordinates. By construction the indexed entries
    // (`pos_map[range.start]` / `pos_map[range.end]`) are in-range —
    // `range.end` is at most `normalized.len()`, for which `pos_map`
    // carries the `text.len()` sentinel, and `range.start` is at least 0.
    let mapped = deduped
        .into_iter()
        .map(|mut h| {
            h.range = pos_map[h.range.start]..pos_map[h.range.end];
            h
        })
        .collect();
    mapped
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
///
/// RENG-113 round 2: the gate does **not** carry the rendered body — the
/// body is the very thing the gate is refusing, and putting it on the
/// error path means the next layer that logs / serializes / displays the
/// `Debug` impl on this type prints plaintext. The fingerprint the error
/// names is the join key: the operator greps the local report file for
/// the 8-hex-char prefix and lands on the offending line.
///
/// `Debug` is implemented by hand (not derived): the only field is `hits`,
/// and a derived `Debug` would delegate to [`SecretHit`]'s derived `Debug`,
/// which would have printed the plaintext `value` field. With the manual
/// `Debug` on `SecretHit`, the `PublishRefusal` `Debug` is safe by
/// construction — `hits` are rendered through the redacted form (kind,
/// key, length, fingerprint prefix, range — no value). Any logger or
/// error formatter that handles `PublishRefusal` via `{:?}` therefore
/// surfaces the same safe join key the explicit [`crate::format_publish_refusal`]
/// does.
#[derive(Clone, PartialEq, Eq)]
pub struct PublishRefusal {
    /// Up to [`PUBLISH_REFUSAL_HITS`] hits, in source order.
    pub hits: Vec<SecretHit>,
}

impl std::fmt::Debug for PublishRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Delegate to `SecretHit`'s manual Debug — it never prints the
        // plaintext value, only kind / key / len / fingerprint / range.
        f.debug_struct("PublishRefusal").field("hits", &self.hits).finish()
    }
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
    Err(PublishRefusal { hits: shown })
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

    // ── RENG-113 round 2: marker awareness + redact idempotence ────────

    /// RENG-113 round 2 (P1): a redacted body re-scanned must produce zero
    /// hits — the marker is structurally a `kind=…,len=…,fp=…` field pair
    /// that the password / token / etc. structured-assignment detector would
    /// otherwise match as if the marker were a fresh secret. Without the
    /// marker-skip, the publish gate refuses every board that carries a
    /// marker, and the renderer is also rejected by the gate it is supposed
    /// to feed.
    #[test]
    fn scan_does_not_re_match_the_redaction_marker_itself() {
        for body in [
            r#"password = "hunter2_real_secret""#,
            r#"api_key = "sk-live-XXXXXXXXXXXXXXXXXXXX""#,
            r#"token = "glpat-AbCdEfGhIjKlMnOpQrStUv""#,
            "see https://alice:s3cret@example.com/data",
            "Authorization: Bearer ghp_abcdefghijklmnopqrstuvwxyz0123456789",
        ] {
            let redacted = redact_text(body);
            assert!(
                scan_for_publish(&redacted).is_ok(),
                "redacted body must not trip the publish gate; got redaction: {redacted:?}"
            );
            assert_eq!(
                scan_secrets(&redacted).len(),
                0,
                "a redacted body must scan to zero hits: {redacted}"
            );
        }
    }

    /// RENG-113 round 2: re-running the redact pass must not modify the
    /// output. The redact→scan→redact loop is what would otherwise keep
    /// growing the marker (each pass wraps the marker in another marker,
    /// because the structured-assignment detectors see the marker's own
    /// `kind=…,len=…,fp=…` body as a value).
    #[test]
    fn redact_text_is_idempotent() {
        let cases = [
            r#"password = "hunter2_real_secret""#,
            "see https://alice:s3cret@example.com/data",
            "Authorization: Bearer ghp_abcdefghijklmnopqrstuvwxyz0123456789",
            "multiple credentials in one body: password = \"hunter2_real\" api_key = \"sk-live-XXXXXXXXXXXXXXXXXXXX\"",
        ];
        for body in cases {
            let once = redact_text(body);
            let twice = redact_text(&once);
            assert_eq!(once, twice, "redact must converge: once={once:?} twice={twice:?}");
        }
    }

    /// RENG-113 round 2 (false-positive case): a value the structured
    /// detectors *would* flag but that is not actually a secret (a
    /// reference to a key, an environment lookup, an injected token)
    /// still must not block --publish. The redaction masks the line
    /// (a false positive marker is cheaper than a leaked secret), and the
    /// marker must then be invisible to the gate.
    #[test]
    fn publish_gate_accepts_a_board_redacted_from_false_positives() {
        let body = "\
            config reading:\n\
            api_key = process.env.API_KEY\n\
            token = os.getenv(\"TOKEN\")\n\
            password = user_input\n";
        let redacted = redact_text(body);
        // All three lines redaction-falsely-fire on the structured detector
        // (the keys match the structured vocabulary). Markers are emitted.
        assert!(redacted.contains("kind=api_key"));
        assert!(redacted.contains("kind=token"));
        assert!(redacted.contains("kind=password"));
        // And the gate is happy — the markers don't re-fire.
        assert!(
            scan_for_publish(&redacted).is_ok(),
            "false-positive mask must not leak as a gate hit"
        );
    }

    /// RENG-113 round 2: marker spans are found correctly across all
    /// structured + value-only kinds. Used to keep `marker_spans` honest
    /// even if the placeholder format ever changes.
    #[test]
    fn marker_spans_find_every_marker_in_a_mixed_body() {
        let body = "\
            password = \"***[redacted:kind=password,len=12,fp=0123456789abcdef,key=password]***\"\n\
            api_key = \"***[redacted:kind=api_key,len=27,fp=0123456789abcdef,key=api_key]***\"\n\
            see https://example.com/***[redacted:kind=basic_auth_url,len=6,fp=0123456789abcdef]***@host\n";
        let spans = marker_spans(body);
        assert_eq!(spans.len(), 3, "every ***[redacted:…]*** is a marker: {body}");
    }

    // ── RENG secrets Debug + JSON-escape redaction (W14) ─────────────

    /// `SecretHit`'s derived `Debug` used to print the plaintext `value`
    /// field verbatim — any `tracing::warn!("hit={:?}", hit)` /
    /// `anyhow!("{:?}", hit)` / panic payload that handled a hit leaked
    /// the credential into the log path. The manual `Debug` impl
    /// exposes kind + key + length + 8-hex fingerprint prefix + byte
    /// range, and never the plaintext. This test pins that contract.
    #[test]
    fn secrethit_debug_does_not_leak_plaintext_value() {
        let plaintext = "hunter2_real_secret_p4ssw0rd";
        let hit = SecretHit {
            kind: SecretKind::Password,
            key: Some("password".to_string()),
            value: plaintext.to_string(),
            range: 0..plaintext.len(),
        };
        let dbg = format!("{hit:?}");
        assert!(
            !dbg.contains(plaintext),
            "Debug must never echo the plaintext value, got: {dbg}"
        );
        // Operator-visible contract: every redacted-form field is in the
        // Debug, the plaintext isn't.
        assert!(dbg.contains("kind"), "Debug shows the kind: {dbg}");
        assert!(dbg.contains("key"), "Debug shows the key field: {dbg}");
        assert!(dbg.contains("password"), "Debug shows the key name: {dbg}");
        assert!(dbg.contains("len"), "Debug shows the length field: {dbg}");
        assert!(
            dbg.contains(&hit.fingerprint_short()),
            "Debug shows the 8-hex fingerprint prefix: {dbg}"
        );
        assert!(dbg.contains("range"), "Debug shows the byte range: {dbg}");
        assert!(dbg.contains("SecretHit"), "Debug shows the type name: {dbg}");
    }

    /// `SecretHit::fingerprint_short` is the join key the operator greps
    /// for — and it is, by construction, a fingerprint of the value,
    /// not the value itself. The Debug output must include only the
    /// fingerprint and never any substring of the value, even if the
    /// value happens to be unique enough to look like a fingerprint.
    #[test]
    fn secrethit_debug_value_substring_can_not_appear() {
        // A value that overlaps with its own fingerprint prefix in
        // appearance: a long string of hex-ish characters whose SHA-256
        // prefix could in principle collide with a substring.
        let plaintext = "abcdef0123456789abcdef0123456789abcdef0123456789";
        let hit = SecretHit {
            kind: SecretKind::Token,
            key: None,
            value: plaintext.to_string(),
            range: 0..plaintext.len(),
        };
        let dbg = format!("{hit:?}");
        // Even the *first 8 chars* of the value must not appear in Debug,
        // because that's enough to narrow a targeted attack against a
        // given value. The fingerprint is the only thing the Debug may
        // surface that touches the value's bytes.
        assert!(
            !dbg.contains(&plaintext[..8]),
            "Debug must not echo any 8-char prefix of the value: {dbg}"
        );
    }

    /// `PublishRefusal::Debug` was derived — and with a derived
    /// `SecretHit::Debug`, every hit's plaintext value landed in the
    /// publish-time error's Debug rendering. The manual `Debug` here
    /// delegates to `SecretHit`'s safe form, so `format!("{:?}", refusal)`
    /// is safe to put in any error / log path without leaking.
    #[test]
    fn publishrefusal_debug_does_not_leak_plaintext_values() {
        let plaintext_a = "hunter2_real_secret_p4ssw0rd";
        let plaintext_b = "sk_live_thisShouldNotLeak_AAAAAAAAA";
        let refusal = PublishRefusal {
            hits: vec![
                SecretHit {
                    kind: SecretKind::Password,
                    key: Some("password".to_string()),
                    value: plaintext_a.to_string(),
                    range: 0..plaintext_a.len(),
                },
                SecretHit {
                    kind: SecretKind::LlmApiKey,
                    key: None,
                    value: plaintext_b.to_string(),
                    range: 0..plaintext_b.len(),
                },
            ],
        };
        let dbg = format!("{refusal:?}");
        assert!(
            !dbg.contains(plaintext_a),
            "PublishRefusal Debug must not leak the password value: {dbg}"
        );
        assert!(
            !dbg.contains(plaintext_b),
            "PublishRefusal Debug must not leak the API key value: {dbg}"
        );
        // Operator-visible join keys survive.
        assert!(dbg.contains("PublishRefusal"), "type name shown: {dbg}");
        assert!(dbg.contains("kind"), "kind shown: {dbg}");
        assert!(
            dbg.contains(&refusal.hits[0].fingerprint_short()),
            "first-hit fingerprint shown: {dbg}"
        );
        assert!(
            dbg.contains(&refusal.hits[1].fingerprint_short()),
            "second-hit fingerprint shown: {dbg}"
        );
    }

    /// W14: a JSON-escaped quoted structured assignment
    /// (`{\"password\": \"hunter2_real\"}`) used to slip the gate
    /// because the structured detector's value char class excluded
    /// `"` and the regex started capturing at the single `\` (below
    /// `MIN_VALUE_LEN`). After pre-normalization of `\"` → `"` (and
    /// the optional-quote wraparound on the structured regex), the
    /// detector matches and the value is masked.
    #[test]
    fn redact_text_catches_json_escaped_value() {
        let body = r#"{\"password\": \"hunter2_real\"}"#;
        let redacted = redact_text(body);
        assert!(!redacted.contains("hunter2_real"), "plaintext must be gone: {redacted}");
        assert!(
            redacted.contains("kind=password"),
            "the structured detector matches after normalization: {redacted}"
        );
        assert!(
            redacted.contains("key=password"),
            "the key name is preserved across the JSON quoting: {redacted}"
        );
        assert!(
            redacted.starts_with("{\\\"password\\\": \\\""),
            "opening JSON framing is preserved verbatim: {redacted}"
        );
        assert!(
            redacted.ends_with("\\\"}"),
            "closing JSON framing is preserved verbatim: {redacted}"
        );
    }

    /// W14 companion for the publish-time gate (`scan_for_publish`),
    /// which shares `scan_secrets` and therefore inherits the
    /// normalization: the gate fails closed on the JSON-escaped form.
    #[test]
    fn scan_for_publish_catches_json_escaped_value() {
        let body = r#"{\"password\": \"hunter2_real\"}"#;
        let refusal = scan_for_publish(body).expect_err("gate must refuse on a JSON-escaped hit");
        assert_eq!(refusal.hits.len(), 1);
        assert_eq!(refusal.hits[0].kind, SecretKind::Password);
        assert_eq!(refusal.hits[0].key.as_deref(), Some("password"));
        assert_eq!(refusal.hits[0].value, "hunter2_real");
    }

    /// W14: idempotence must hold on the escaped form, just as it does
    /// on the unescaped form. The first redaction produces a marker in
    /// the original text; re-running the pass on the result must find
    /// zero hits and produce the same text (the marker itself has no
    /// `\"` to normalize further, so the second pass is a no-op).
    #[test]
    fn redact_text_is_idempotent_on_json_escaped_input() {
        let body = r#"{\"password\": \"hunter2_real\"}"#;
        let once = redact_text(body);
        assert!(!once.contains("hunter2_real"), "plaintext is gone: {once}");
        assert!(once.contains("***[redacted:"), "marker is in place: {once}");
        let twice = redact_text(&once);
        assert_eq!(
            once, twice,
            "redact must converge on the escaped form: once={once:?} twice={twice:?}"
        );
    }

    /// W14: ranges coming back from `scan_secrets` must point into the
    /// *original* text (the bytes that produced the hit), not the
    /// normalized `\"` → `"` form — otherwise `redact_text`'s `&text[..]`
    /// slicing would substitute over the wrong bytes and corrupt the
    /// surrounding JSON framing.
    #[test]
    fn scan_secrets_ranges_map_back_to_original_bytes() {
        // Construct a value with deliberately unique characters so any
        // byte-range misfire shows up as a substitution over the wrong
        // bytes (would produce garbled JSON framing in the redacted
        // output).
        let body = r#"{\"password\": \"ZZUNIQUE_MARKER_VALUE_42\"}"#;
        let hits = scan_secrets(body);
        assert_eq!(hits.len(), 1, "exactly one structured hit: {hits:?}");
        let hit = &hits[0];
        // The reported range, sliced on the ORIGINAL text, must yield
        // the same value the detector stored on the hit (any byte drift
        // would leave a leftover plaintext fragment or substitute over
        // the JSON framing).
        let original_slice = &body[hit.range.clone()];
        assert_eq!(
            original_slice, "ZZUNIQUE_MARKER_VALUE_42",
            "hit range must point at the original-text bytes of the value, got slice={original_slice:?}"
        );
        // And the redacted output must keep the JSON framing intact
        // around the substituted placeholder. Use regular-string
        // framing literals (with `\\`/`\"` escapes) — simpler than
        // raw-string forms whose terminator interacts with the
        // embedded `\"` sequence.
        let redacted = redact_text(body);
        assert!(
            !redacted.contains("ZZUNIQUE_MARKER_VALUE_42"),
            "the unique value is masked: {redacted}"
        );
        assert!(
            redacted.starts_with("{\\\"password\\\": \\\"***["),
            "opening framing + marker prefix preserved: {redacted}"
        );
        // Marker format is `***[redacted:…]***`, so the marker ends
        // with `***` (three asterisks), not `***]`. After the marker
        // comes the closing JSON framing `\"}`.
        assert!(
            redacted.ends_with("***\\\"}"),
            "marker suffix + closing framing preserved: {redacted}"
        );
    }

    /// W14: a value-only-shaped provider token inside a JSON-escaped
    /// payload is masked regardless of whether a structured `key=value`
    /// assignment surrounds it. The value-only patterns rely on byte
    /// boundaries (`\b...\b`), not on quoted framing, so the `\"`
    /// normalization changes nothing for them — but it must not break
    /// them either. This test pins that the value-only path stays
    /// correct through the normalization: a non-token-shaped key
    /// (`description`) keeps the structured `token=` detector out of
    /// the picture so the value-only GitLab pattern is what fires.
    #[test]
    fn redact_catches_value_only_inside_json_escaped_object() {
        let body = r#"{\"description\": \"leaked glpat-AbCdEfGhIjKlMnOpQrStUv during incident\"}"#;
        let redacted = redact_text(body);
        assert!(
            !redacted.contains("glpat-AbCdEfGhIjKlMnOpQrStUv"),
            "the value-only GitLab token is masked: {redacted}"
        );
        assert!(
            redacted.contains("kind=gitlab_token"),
            "the value-only path picks up the prefix inside the JSON-escaped payload: {redacted}"
        );
    }

    /// W14 robustness: a single trailing `\` (no follow-on `"`) is
    /// not a JSON escape and must be preserved verbatim by the
    /// normalization helper — only the literal two-byte sequence `\"`
    /// unescapes to `"`.
    #[test]
    fn unescape_quotes_does_not_strip_lone_backslash() {
        let (normalized, _map) = unescape_quotes_for_scan(r"end with \");
        assert_eq!(normalized, r"end with \", "lone \\ is left alone");
    }

    /// W14 robustness: a backslash followed by a non-quote (e.g. `\n`)
    /// is also not a JSON-escape-of-a-quote; the helper must leave it
    /// alone (other escape sequences are out of scope for W14 — only
    /// `\"` is normalized; see the finding's bounded instruction).
    #[test]
    fn unescape_quotes_does_not_strip_other_escapes() {
        let (normalized, _map) = unescape_quotes_for_scan(r"line\nbreak");
        assert_eq!(normalized, r"line\nbreak", "\\n is left as-is");
    }
}
