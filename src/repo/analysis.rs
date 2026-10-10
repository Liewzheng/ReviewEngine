use std::path::Path;

use super::experts::facts::{cfg_test_line_spans, is_test_file};
use super::FileEntry;

/// Classification of a [`SecurityFinding`].
///
/// Mechanical findings come from regex pattern matching against credential
/// shapes (api keys, passwords, tokens, private keys). They are
/// syntax-driven: high false-positive rate on test fixtures, placeholders
/// and format templates, so by policy their weight on the security score
/// must be capped — see [`scan_security_patterns`] and the security expert
/// scoring in `static_experts::security`.
///
/// Semantic findings are produced by code-level reasoning (typically an
/// LLM): output-channel leaks, proxy-bypass evasions, weak temp-file
/// permissions, tokens in query/path beyond `userinfo`, …  These need
/// structural understanding of the program rather than a regex sweep, so
/// they are not produced here. The kind tag is reserved so callers can
/// attach semantic findings to the same flow without a duplicate type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindingKind {
    /// Regex-driven secret-detection hit.
    Mechanical,
    /// Code-reasoning hit produced by an LLM expert (reserved for future use).
    Semantic,
}

impl FindingKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FindingKind::Mechanical => "mechanical",
            FindingKind::Semantic => "semantic",
        }
    }
}

/// A potential security issue found via pattern matching (mechanical) or
/// code-level reasoning (semantic).
#[derive(Debug, Clone)]
pub struct SecurityFinding {
    pub file: String,
    pub pattern: String,
    pub line: usize,
    pub severity: String,
    /// Whether the hit came from a mechanical (regex) scan or a semantic
    /// (LLM) expert. See [`FindingKind`].
    pub kind: FindingKind,
}

/// Scan files for common security patterns (API keys, passwords, etc).
///
/// Mechanical hits in test files, on placeholder values, and in
/// documentation/comment lines are suppressed before reporting — they
/// would otherwise flood the report with fixture noise (test inputs,
/// formatting examples, illustrative values wrapped in `〈…〉`, and
/// prose that describes the regex itself) and drown the semantic
/// security signal users actually care about. The cap on mechanical
/// contribution to the security score lives in
/// `static_experts::security`.
pub fn scan_security_patterns(entries: &[FileEntry]) -> Vec<SecurityFinding> {
    let mut findings = Vec::new();

    for entry in entries {
        if entry.is_binary || entry.is_generated {
            continue;
        }
        if is_test_file_path(&entry.path) {
            // Test files are full of mock credentials and fixtures; even
            // a careful regex sweep would report them all as "secrets".
            continue;
        }
        if is_markdown_path(&entry.path) {
            // Markdown documents are prose: CHANGELOG / README bullets
            // routinely describe the regex itself in plain text, e.g.
            // `` `password = "…"` `` in `CHANGELOG.md:13` and
            // `` `password: "sk"` `` in `CHANGELOG.md:4`, and those
            // lines would otherwise fire as `[mechanical]`. The scan
            // is for source code, not for documentation; real secrets
            // embedded in markdown are a doc hygiene issue (M28
            // RENG-114 follow-up).
            continue;
        }
        if let Ok(content) = std::fs::read_to_string(&entry.path) {
            findings.extend(scan_security_patterns_in_text(&entry.path, &content));
        }
    }

    findings
}

/// Markdown document path: the `.md` / `.markdown` extension (case
/// insensitive). Cheap extension check; no MIME sniffing — the scan is
/// for source code, and the only place these extensions show up in a
/// source tree is documentation files.
fn is_markdown_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".md") || lower.ends_with(".markdown")
}

/// Vue single-file component path: the `.vue` extension. The Vue
/// `<template>` block uses directive bindings like `:prop="…"` and
/// `v-bind:prop="…"` where the bound value is a JS expression (often
/// a function call), not a string literal credential — these are
/// distinguished from static attributes (which always carry literal
/// values) by the LHS starting with `:` or `v-bind:`; see
/// [`is_vue_directive_binding`] and [`scan_security_patterns_in_text`].
fn is_vue_path(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("vue"))
}

/// Rust source file path: the `.rs` extension. Used to narrow the
/// `api.?key` identifier-only suppression (see
/// [`scan_security_patterns_in_text`]) so config formats
/// (`.yaml` / `.yml` / `.toml` / `.json`) where the same shape is a
/// real hardcoded credential keep reporting.
fn is_rust_source_path(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("rs"))
}

/// Wrapper around [`super::experts::facts::is_test_file`] using the path's
/// file name, kept local so callers don't have to know the facts module's
/// signature. Reused by [`scan_security_patterns_in_text`].
fn is_test_file_path(path: &str) -> bool {
    let name = Path::new(path).file_name().and_then(|n| n.to_str()).unwrap_or("");
    is_test_file(name, path)
}

fn build_security_regexes() -> Vec<(regex::Regex, &'static str)> {
    let sensitive_patterns = [
        (r#"api.?key\s*[:=]\s*['"]?[A-Za-z0-9_]{16,}"#, "Possible API key"),
        (r"sk-[A-Za-z0-9]{20,}", "Possible secret key"),
        (r#"password\s*[:=]\s*['"][^'"]+['"]"#, "Hardcoded password"),
        (r#"token\s*[:=]\s*['"][A-Za-z0-9_]{20,}['"]"#, "Possible token"),
        (r"-----BEGIN (RSA |EC )?PRIVATE KEY-----", "Private key"),
    ];

    sensitive_patterns
        .iter()
        .filter_map(|(pattern, desc)| regex::Regex::new(pattern).ok().map(|r| (r, *desc)))
        .collect()
}

/// A matched token is a placeholder/example (e.g. `sk-xxxx…xxxx` shipped in
/// sample config or docs), not a real secret, when the value after any
/// structural prefix (e.g. the `sk-` of the secret-key pattern) is composed
/// solely of placeholder characters: `x`/`X`/`*`/`-`/`_`. Real secrets have
/// entropy; dummy strings like `sk-xxxxxxxxxxxxxxxx` or `****` do not.
fn is_placeholder_token(token: &str) -> bool {
    let value = token.strip_prefix("sk-").unwrap_or(token);
    !value.is_empty() && value.chars().all(|c| matches!(c, 'x' | 'X' | '*' | '-' | '_'))
}

/// Detect placeholder text wrapped in angle brackets (Chinese typewriter
/// `〈…〉` or ASCII `<…>`).
///
/// Documentation samples often look like `password="〈写账号密码〉"` or
/// `password="<YOUR_KEY_HERE>"` — those values are not real secrets, but
/// the regex would happily match the surrounding `password=` prefix and
/// report a hit. Real credentials rarely if ever contain literal angle
/// brackets, so any `<` / `>` / `〈` / `〉` character inside the matched
/// span is treated as a placeholder marker.
fn is_placeholder_bracket(line: &str, match_start: usize, match_end: usize) -> bool {
    let matched = &line[match_start..match_end];
    matched.contains(['<', '>', '〈', '〉'])
}

/// Scan a single text block for security patterns. Exposed for unit testing.
///
/// Every hit is tagged [`FindingKind::Mechanical`]. The following
/// categories are suppressed before they reach the report:
///
/// - **Test files** — paths recognised by [`super::experts::facts::is_test_file`]
///   (`tests/*`, `*_test.rs`, `*.spec.ts`, `__tests__/*`, …). Fixtures
///   are full of mock credentials; a regex sweep would flag them all.
///   Belt-and-braces with the entry-level skip in
///   [`scan_security_patterns`].
/// - **Markdown documents** — paths ending in `.md` / `.markdown`.
///   CHANGELOG / README bullets routinely describe the regex itself
///   in plain text, e.g. `` `password: "sk"` `` in `CHANGELOG.md:4` or
///   `` `password = "…"` `` in `CHANGELOG.md:13`; those lines would
///   otherwise fire as `[mechanical]`. The scan is for source code
///   (M28 RENG-114 follow-up).
/// - **`//` line comments** — lines that, trimmed, start with `//`.
///   Covers regular `//` (Rust / JS / TS), doc `///` and module-level
///   `//!` (Rust). Docstrings commonly embed examples like
///   `password = "real_password"` to illustrate the redaction format
///   or the regex shape; those examples are not real credentials
///   (M28). The predicate is **language-agnostic** — any line whose
///   first non-whitespace chars are `//` is skipped, regardless of
///   the file's language. Python (`#`) and shell / YAML (`#`) are
///   not affected.
/// - **`#[cfg(test)] mod … { … }` blocks** inside regular `.rs` files —
///   inline test modules that the path-based test-file rule cannot see
///   (RENG-114 follow-up, sirena reproducer).
/// - **Placeholder tokens** — values composed of `x`/`X`/`*`/`-`/`_`
///   after any structural prefix (`sk-…`), or values wrapped in
///   angle brackets (`〈…〉`, `<…>`). Dummy/example values, not real
///   secrets.
/// - **Vue directive bindings** — `.vue` files where the LHS
///   attribute name starts with `:` (shorthand) or `v-bind:` (long
///   form). The bound value is a Vue expression (typically a
///   function call like `showPasswordFor(draft.token)`, sometimes an
///   identifier reference), not a string literal credential.
///   Discriminator: walk the LHS back from `mat_start` through
///   identifier chars (including `-`, the Vue prop-name infix)
///   until a whitespace or tag char; the result is the LHS, and a
///   `:` / `v-bind:` prefix marks a binding. Static attributes like
///   `show-password="hunter2_real"` (no directive prefix) fall
///   through and report (M28).
/// - **Identifier-only `api.?key` matches in `.rs` source files** —
///   when the regex consumed a `:` separator AND no opening quote,
///   in a file whose extension is `.rs`. The matched value is a Rust
///   struct-literal field initializer whose RHS is an identifier
///   reference (`api_key: encrypt_non_empty(…)` in
///   `src/store/rows.rs:143`, the M28 reproducer), not a hardcoded
///   literal. The suppression is **narrowed to `.rs`** so config
///   formats (`.yaml` / `.yml` / `.toml` / `.json`) keep reporting
///   the same shape — in YAML/TOML/JSON an unquoted
///   `api_key: AKIAIOSFODNN7EXAMPLE` is a real hardcoded credential,
///   not a struct literal. Quoted struct literals (`api_key: "…"`),
///   assignments (`api_key = "…"` / `api_key = someVar`), and all
///   matches in non-`.rs` config files keep reporting (M28).
///
/// All seven layers compose: a production `api_key = "secret"` line
/// inside a regular `.rs` file outside any `#[cfg(test)]` block still
/// surfaces, while every reproducer from the M25 self-audit (CHANGELOG
/// prose / docstring examples / Vue directive bindings / struct-field
/// function calls) is suppressed. Static Vue attributes with literal
/// values, and unquoted `api_key:` keys in YAML/TOML/JSON, are NOT
/// affected by the suppressions and continue to report.
pub(crate) fn scan_security_patterns_in_text(file: &str, content: &str) -> Vec<SecurityFinding> {
    let re_list = build_security_regexes();
    let mut findings = Vec::new();
    let skip_test = is_test_file_path(file);
    let skip_markdown = is_markdown_path(file);
    let is_vue = is_vue_path(file);
    // Inline `#[cfg(test)] mod … { … }` blocks live inside regular
    // source files, so `is_test_file_path` cannot see them. The span
    // list is computed once per file (a brace-counting pass) and
    // every line is tested against it — the cost is O(L + B·K)
    // where L is the line count, B is the block count and K is the
    // average block length; for sirena's `upload.rs` (1257 lines, 1
    // block) it is one comparison per regex-bearing line.
    let cfg_test_spans = cfg_test_line_spans(content);

    for (i, line) in content.lines().enumerate() {
        if skip_test || skip_markdown {
            // Test files and Markdown documents are skipped wholesale:
            // every line is exempt, regardless of what it carries.
            // Test fixtures are mock credentials; markdown is prose.
            // The same exclusions are enforced at the entry level by
            // `scan_security_patterns`, so a file the regex never
            // reaches is also never inspected line by line — the two
            // layers are belt-and-braces for the same reason.
            continue;
        }
        if is_double_slash_line_comment(line) {
            // Rust line comments (`//`, `///`, `//!`) describe code or
            // carry examples, not real credentials. The 13 M28
            // reproducer sites in `src/models/secrets.rs` /
            // `src/cli/handlers/output.rs` /
            // `src/output/renderer.rs` /
            // `src/output/team_renderer.rs` /
            // `src/repo/experts/facts.rs` are all line comments
            // embedded in the source tree as documentation of the
            // redaction / fixture shapes.
            continue;
        }
        let line_no = i + 1;
        if cfg_test_spans.iter().any(|&(s, e)| line_no >= s && line_no <= e) {
            // Line is inside an inline `#[cfg(test)] mod … { … }`
            // block — treat it the same as a dedicated test file.
            // Without this layer, sirena's `upload.rs` lines 673 /
            // 741 / 843 / 994 (every one inside `mod tests { … }`)
            // fire as `[mechanical] Hardcoded password` and the
            // security score drops by 16 points (4 × MECHANICAL_PER_HIT).
            continue;
        }
        for (re, desc) in &re_list {
            for mat in re.find_iter(line) {
                if is_placeholder_token(mat.as_str()) {
                    continue; // example/dummy placeholder, not a real secret
                }
                if is_placeholder_bracket(line, mat.start(), mat.end()) {
                    continue; // `password=〈写账号密码〉` style placeholder
                }
                if is_vue && is_vue_directive_binding(line, mat.start()) {
                    // Vue directive binding (`:prop="…"` /
                    // `v-bind:prop="…"`): the LHS attribute name
                    // starts with `:` (shorthand) or `v-bind:`
                    // (long form), so the bound value is a Vue
                    // expression — typically a function call like
                    // `showPasswordFor(draft.token)`, sometimes an
                    // identifier reference like
                    // `someLongIdentifier1234`. Not a string literal
                    // credential; suppress.
                    //
                    // A static attribute like
                    // `show-password="hunter2_real"` (no `:` /
                    // `v-bind:` prefix, hyphenated name, literal
                    // value) does **not** trigger this skip — the
                    // LHS attribute name starts with `s`, not `:`
                    // or `v-bind:`, so it falls through and the
                    // literal value is reported.
                    continue;
                }
                if *desc == "Possible API key"
                    && is_rust_source_path(file)
                    && first_separator_is_colon(&mat)
                    && !consumed_opening_quote(&mat)
                {
                    // `api.?key` pattern, struct-field shape in a
                    // `.rs` source file: separator is `:` (Rust
                    // struct literal) AND no opening quote was
                    // consumed. The matched value is an identifier
                    // reference, e.g.
                    // `api_key: encrypt_non_empty(…)` in
                    // `src/store/rows.rs:143` (the M28 reproducer) —
                    // not a hardcoded literal.
                    //
                    // The suppression is **narrowed to `.rs`** so
                    // config formats (`.yaml` / `.yml` / `.toml` /
                    // `.json`) keep reporting the same shape — in
                    // YAML/TOML/JSON an unquoted
                    // `api_key: AKIAIOSFODNN7EXAMPLE` is a real
                    // hardcoded credential, not a struct literal.
                    // Quoted struct literals (`api_key: "secret"`)
                    // keep reporting because the regex consumed an
                    // opening quote; `api_key = "…"` /
                    // `api_key = someVar` keep reporting because the
                    // separator is `=` (real assignment form).
                    continue;
                }
                findings.push(SecurityFinding {
                    file: file.to_string(),
                    pattern: desc.to_string(),
                    line: line_no,
                    severity: "medium".to_string(),
                    kind: FindingKind::Mechanical,
                });
                break; // one finding per (line, pattern) is enough
            }
        }
    }

    findings
}

/// A line whose trimmed text begins with `//` is a line-comment in any
/// of the line-comment-syntax languages we scan: regular `//` (Rust,
/// JS/TS), doc `///` and module-level `//!` (Rust), and `//` comments
/// (TS / Vue `.vue` `<script>` blocks — Vue templates use HTML
/// comment syntax, not `//`). Python (`#`) and shell / YAML (`#`) use
/// different line-comment markers and are not affected by this
/// predicate. The M28 reproducer corpus uses the Rust forms; the
/// predicate is language-agnostic so a future scanner over a TS
/// codebase picks up `//` doc comments for free.
fn is_double_slash_line_comment(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("//")
}

/// True if the regex match sits inside a Vue directive binding in a
/// `.vue` file's `<template>` block. The LHS attribute name is
/// reconstructed by walking back from `mat_start` through identifier
/// chars (including `-`, the Vue prop-name infix) until a whitespace
/// or HTML/Vue tag char is hit; the LHS starts with `:` (shorthand)
/// or `v-bind:` (long form) iff the attribute is a directive binding
/// whose value is a JS expression rather than a static literal.
///
/// Walk-back bounds: stop chars are whitespace, `<` (open tag),
/// `>` (close tag), `/` (self-close), and any of `'` / `"` (Vue
/// attribute quotes); `:` is **not** a stop char so a multi-segment
/// hyphenated LHS like `:show-password` correctly yields the full
/// LHS `:show-password`, not just `-password`.
///
/// Discriminator for three M28 shapes:
/// - `:show-password="fn(…)"` → LHS `:show-password` starts with
///   `:` → directive binding → suppress.
/// - `v-bind:password="fn(…)"` → LHS `v-bind:password` starts with
///   `v-bind:` → directive binding → suppress.
/// - `:api-key="identifier1234"` → LHS `:api-key` starts with `:` →
///   directive binding → suppress.
/// - `show-password="hunter2_real"` → LHS `show-password` starts
///   with `s` → static attribute (no directive prefix) → does NOT
///   trigger the skip, the literal value is reported.
fn is_vue_directive_binding(line: &str, mat_start: usize) -> bool {
    let bytes = line.as_bytes();
    let mut i = mat_start;
    while i > 0 {
        let c = bytes[i - 1];
        if c.is_ascii_whitespace() || c == b'<' || c == b'>' || c == b'/' || c == b'\'' || c == b'"' {
            break;
        }
        i -= 1;
    }
    let lhs = &line[i..mat_start];
    lhs.starts_with(':') || lhs.starts_with("v-bind:")
}

/// For the `api.?key` pattern, returns `true` iff the regex consumed a
/// `:` separator (vs. `=`). The regex is
/// `api.?key\s*[:=]\s*['"]?[A-Za-z0-9_]{16,}` — the keyword is at
/// the start, optional whitespace, then `:` or `=`. Walking forward
/// from the start of the match, the first `:` / `=` byte is the
/// separator (the regex's `\s*` consumes any leading whitespace,
/// and `:` / `=` are not valid Rust identifier chars so a `:` / `=`
/// inside the LHS keyword is impossible).
///
/// Used to distinguish struct-field syntax (`api_key: …`) from a
/// real assignment (`api_key = …`) without touching quoted vs.
/// unquoted value shapes.
fn first_separator_is_colon(mat: &regex::Match) -> bool {
    for &b in mat.as_str().as_bytes() {
        if b == b':' {
            return true;
        }
        if b == b'=' {
            return false;
        }
    }
    false
}

/// For the `api.?key` pattern, returns whether the regex consumed an
/// opening quote (`'` or `"`) before the value. The regex is
/// `api.?key\s*[:=]\s*['"]?[A-Za-z0-9_]{16,}` — after the LHS
/// keyword + separator there is optional whitespace and an optional
/// quote. The value (`[A-Za-z0-9_]{16,}`) is contiguous identifier
/// chars at the end of the match, so walking back from the end
/// skipping identifier chars lands on either:
///
/// - the opening quote (`"` / `'`), if one was consumed — the value
///   is a string literal credential;
/// - whitespace (or the separator itself), if no quote was consumed
///   — the value is an identifier reference (e.g.
///   `encrypt_non_empty(…)`) and must not be reported as a hardcoded
///   API key.
fn consumed_opening_quote(mat: &regex::Match) -> bool {
    let bytes = mat.as_str().as_bytes();
    let mut i = bytes.len();
    while i > 0 && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_') {
        i -= 1;
    }
    if i == 0 {
        return false;
    }
    let prev = bytes[i - 1];
    prev == b'\'' || prev == b'"'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_security_patterns_in_text_finds_api_key() {
        // Build a 16+ alphanumeric token at runtime so the source file
        // itself does not contain a string that matches the API key regex.
        let key = "a".repeat(16);
        let content = format!("{}={}", "api_key", key);
        let findings = scan_security_patterns_in_text("config.env", &content);
        assert!(!findings.is_empty());
        assert!(findings.iter().any(|f| f.pattern == "Possible API key"));
        // Mechanical hits must self-identify so the expert can split them
        // from semantic findings during scoring.
        assert!(
            findings.iter().all(|f| f.kind == FindingKind::Mechanical),
            "regex hits are mechanical"
        );
    }

    #[test]
    fn scan_security_patterns_in_text_finds_hardcoded_password() {
        // Build the assignment at runtime; the literal field name is kept
        // in a separate variable so the source line does not match the regex.
        let field = "password";
        let value = "x".repeat(8);
        let content = format!(r#"{} = "{}""#, field, value);
        let findings = scan_security_patterns_in_text("main.rs", &content);
        assert!(!findings.is_empty());
        assert!(findings.iter().any(|f| f.pattern == "Hardcoded password"));
    }

    #[test]
    fn scan_security_patterns_in_text_finds_secret_key() {
        // Build the sk- token at runtime so it is not present in source.
        let tail = "a".repeat(20);
        let content = format!("{}{}", "sk-", tail);
        let findings = scan_security_patterns_in_text("keys.env", &content);
        assert!(!findings.is_empty());
        assert!(findings.iter().any(|f| f.pattern == "Possible secret key"));
    }

    #[test]
    fn scan_security_patterns_in_text_finds_private_key() {
        // Assemble the PEM header from parts so the literal regex pattern
        // does not appear contiguously in the source file.
        let prefix = "-----BEGIN ";
        let key_type = "RSA ";
        let suffix = "PRIVATE KEY-----";
        let content = format!("{}{}{}\nMIIEpAIBAAKCAQEA...", prefix, key_type, suffix);
        let findings = scan_security_patterns_in_text("key.pem", &content);
        assert!(!findings.is_empty());
        assert!(findings.iter().any(|f| f.pattern == "Private key"));
    }

    #[test]
    fn scan_security_patterns_in_text_reports_correct_line_numbers() {
        let key = "a".repeat(16);
        let content = format!("safe\n{}={}\nsafe again", "api_key", key);
        let findings = scan_security_patterns_in_text("config.env", &content);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].line, 2);
    }

    #[test]
    fn scan_security_patterns_in_text_returns_empty_for_clean_content() {
        let findings = scan_security_patterns_in_text("main.rs", "fn main() { println!(\"hello\"); }");
        assert!(findings.is_empty());
    }

    #[test]
    fn scan_security_patterns_in_text_skips_placeholder_secret_key() {
        // `sk-` + 40 `x`s is a dummy/example token (all placeholder chars),
        // so it must not be reported as a real secret.
        let content = format!("{}{}", "sk-", "x".repeat(40));
        let findings = scan_security_patterns_in_text("config.example.ts", &content);
        assert!(!findings.iter().any(|f| f.pattern == "Possible secret key"));
    }

    #[test]
    fn scan_security_patterns_in_text_finds_high_entropy_secret_key() {
        // A token with diverse chars (upper/lower/digits) after `sk-` is not
        // a placeholder and must still be reported as a possible secret key.
        let tail = "Ab3cD5eF7gH9iJ1kL2mN";
        let content = format!("{}{}", "sk-", tail);
        let findings = scan_security_patterns_in_text("keys.env", &content);
        assert!(findings.iter().any(|f| f.pattern == "Possible secret key"));
    }

    #[test]
    fn scan_security_patterns_in_text_skips_placeholder_api_key() {
        // Sample config line with a placeholder value (`openaiApiKey: 'sk-…'`)
        // must not be reported as a possible API key.
        let content = format!("openaiApiKey: 'sk-{}'", "x".repeat(40));
        let findings = scan_security_patterns_in_text("config.ts", &content);
        assert!(!findings.iter().any(|f| f.pattern == "Possible API key"));
    }

    /// `password="〈写账号密码〉"` is the form RENG-19 highlighted: a
    /// documentation sample with a Chinese-style typewriter-bracket
    /// placeholder. The bracket payload is non-ASCII so the older
    /// `is_placeholder_token` filter (x/X/*/-/_ only) missed it.
    #[test]
    fn scan_security_patterns_in_text_skips_chinese_bracket_placeholder() {
        let real = "Ab3cD5eF7gH9iJ1kL2mN";
        let content = format!(r#"password = "〈{}〉""#, real);
        let findings = scan_security_patterns_in_text("docs/upload.rs", &content);
        assert!(
            findings.is_empty(),
            "Chinese-bracket placeholder is not a real secret: {:?}",
            findings
        );
    }

    /// `password="<YOUR_KEY_HERE>"` is the ASCII-bracket cousin of the
    /// sample above; both bracket forms must be filtered before reaching
    /// the report.
    #[test]
    fn scan_security_patterns_in_text_skips_ascii_bracket_placeholder() {
        let content = r#"password = "<YOUR_PASSWORD_HERE>""#;
        let findings = scan_security_patterns_in_text("docs/sample.env", &content);
        assert!(
            findings.is_empty(),
            "ASCII-bracket placeholder is not a real secret: {:?}",
            findings
        );
    }

    /// Any path recognised by [`crate::repo::experts::facts::is_test_file`]
    /// must be skipped by the scan entirely: test fixtures are full of
    /// mock credentials and would drown the report otherwise.
    #[test]
    fn scan_security_patterns_skips_test_files() {
        let real = "a".repeat(16);
        let content = format!(r#"api_key = "{}""#, real);
        let cases = [
            ("tests/fixtures.rs", 0usize),
            ("src/upload_tests.rs", 0usize),
            ("src/output/parser/tests.rs", 0usize),
            ("src/__tests__/helper.ts", 0usize),
            // Same content in a non-test path must still be flagged.
            ("src/upload.rs", 1usize),
        ];
        for (path, expected) in cases {
            let findings = scan_security_patterns_in_text(path, &content);
            assert_eq!(findings.len(), expected, "test-file exclusion: {path} → {findings:?}");
        }
    }

    /// The aggregated scan over a real entry list must drop every hit
    /// whose path is classified as a test file, while non-test paths
    /// with the same payload still report.
    #[test]
    fn scan_security_patterns_skips_test_entries() {
        let dir = tempfile::tempdir().unwrap();
        let real = "a".repeat(16);
        let body = format!(r#"api_key = "{}""#, real);

        let fixture_path = dir.path().join("tests").join("fixtures.rs");
        let source_path = dir.path().join("src").join("upload.rs");
        std::fs::create_dir_all(fixture_path.parent().unwrap()).unwrap();
        std::fs::create_dir_all(source_path.parent().unwrap()).unwrap();
        std::fs::write(&fixture_path, &body).unwrap();
        std::fs::write(&source_path, &body).unwrap();

        let entries = vec![
            FileEntry {
                path: fixture_path.to_string_lossy().into_owned(),
                language: "Rust".to_string(),
                loc: 1,
                is_binary: false,
                is_generated: false,
            },
            FileEntry {
                path: source_path.to_string_lossy().into_owned(),
                language: "Rust".to_string(),
                loc: 1,
                is_binary: false,
                is_generated: false,
            },
        ];

        let findings = scan_security_patterns(&entries);
        assert_eq!(findings.len(), 1, "exactly one non-test hit expected, got {findings:?}");
        assert!(
            findings[0].file.ends_with("upload.rs"),
            "the surviving hit must be the non-test file: {:?}",
            findings[0]
        );
    }

    /// RENG-114 follow-up (sirena): four password-shaped assignments
    /// inside a regular `.rs` file's inline `#[cfg(test)] mod tests
    /// { … }` block must NOT produce any finding. The file is
    /// production-named (not a test file by path) and every regex
    /// hit is inside the test module — the same shape as the four
    /// lines the sirena repro hit (`crates/sirena-cli/src/upload.rs`
    /// 673 / 741
    /// / 843 / 994, all four are `password: "sk"`-shaped and all four
    /// match the Hardcoded-password regex). Without this pin the
    /// security scan would emit 4 `[mechanical] Hardcoded password`
    /// findings, dragging the security score down by 16 points on a
    /// repo with no real leak. The pin asserts **zero** findings in
    /// the cfg-test block; production lines that carry the same
    /// payload still flag (pinned by
    /// `…_keeps_production_lines_outside_cfg_test`).
    ///
    /// M19 r2 (reviewer-facts P2-1): all four fixture sites are
    /// password-shaped like the sirena reproducer — three of the
    /// four forms the sirena file actually carries (`t.password =`,
    /// struct-literal `password: "sk".into()`, and `.to_string()`)
    /// plus the simpler `let password = "sk";` — so reverting the
    /// cfg-test skip yields exactly 4 findings (one per fixture),
    /// not 1.
    #[test]
    fn scan_security_patterns_in_text_skips_cfg_test_blocks() {
        // Mimic the sirena shape: 4 password-shaped fixture
        // assignments inside an inline `#[cfg(test)] mod tests { … }`,
        // with no `password` / `api_key` / `token` pattern on any
        // production line — so the only regex candidates are inside
        // the cfg-test block, and a clean result proves the skip.
        // Reverting the skip must surface exactly 4 findings, one
        // per fixture line (M19 r2 fixture fix: every fixture line
        // is password-shaped so the pin matches the sirena shape).
        let content = "fn real() { let config_name = \"upload\"; }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn creds() {\n        let password = \"sk\";\n    }\n    #[test]\n    fn another() {\n        t.password = \"sk\".into();\n    }\n    #[test]\n    fn third() {\n        let password = \"sk\".into();\n    }\n    #[test]\n    fn fourth() {\n        let password = \"sk\".to_string();\n    }\n}\n";
        let findings = scan_security_patterns_in_text("src/upload.rs", content);
        assert!(
            findings.is_empty(),
            "inline #[cfg(test)] mod tests blocks must be skipped wholesale, got {findings:?}"
        );
    }

    /// RENG-114 follow-up: a `#[cfg(test)] mod … { … }` block is
    /// skipped, but a regex hit on a production line of the same
    /// file (above the `#[cfg(test)]` opener) must still report.
    /// The two layers (`is_test_file_path` for whole test files,
    /// `cfg_test_line_spans` for inline blocks) must compose so a
    /// regular `.rs` file with a single cfg-test block still surfaces
    /// genuine production leaks.
    #[test]
    fn scan_security_patterns_in_text_keeps_production_lines_outside_cfg_test() {
        let real = "a".repeat(16);
        // Production line carries the credential; inline test block
        // carries a placeholder password (would have hit without the
        // cfg-test skip). The production hit must survive; the test
        // hit must be suppressed.
        let content = format!(
            "fn real() {{ let api_key = \"{real}\"; }}\n\
             \n\
             #[cfg(test)]\n\
             mod tests {{\n\
                 #[test]\n\
                 fn t() {{\n\
                     let password = \"sk\";\n\
                 }}\n\
             }}\n"
        );
        let findings = scan_security_patterns_in_text("src/upload.rs", &content);
        assert_eq!(
            findings.len(),
            1,
            "exactly one production hit must survive, got {findings:?}"
        );
        assert_eq!(findings[0].line, 1, "the surviving hit is the production line");
        assert_eq!(findings[0].pattern, "Possible API key");
    }

    // -----------------------------------------------------------------
    // M28 RENG-114 follow-up: 20 mechanical security FPs from the M25
    // self-audit (`reports/reng-self-audit/raw-report.md` security
    // section + `findings-summary.json` §spot_checks.S5). The five
    // tests below pin the new skip rules (markdown prose, Vue prop
    // bindings, Rust line comments, identifier-only `api.?key`) and
    // the comprehensive corpus test asserts every one of the 20
    // reproducer sites returns zero findings. Real positives (a
    // production-line `api_key = "…"` quoted literal, a literal
    // `:password="…"` Vue prop, a Rust non-comment line with a real
    // password) must still flag — pinned by
    // `…_keeps_real_positives_after_m28_skips`.
    // -----------------------------------------------------------------

    /// Markdown prose: CHANGELOG.md lines 4 / 13 / 41 are bullets
    /// describing the regex itself in plain text. The scan is for
    /// source code, not documentation.
    #[test]
    fn scan_security_patterns_in_text_skips_markdown_prose() {
        let cases: &[&str] = &[
            // CHANGELOG.md:4 — "Inline #[cfg(test)] mod … { … }" bullet
            r#"- **Inline `#[cfg(test)] mod … { … }` blocks … `password = "real_password"` is exactly the**"#,
            // CHANGELOG.md:13 — "The security expert separates mechanical …" bullet
            r#"- **The security expert separates … `password = "…"` …**"#,
            // CHANGELOG.md:41 — "`Evidence` blocks are redacted …" bullet
            r#"- **`Evidence` blocks are redacted … `password = "…"` …**"#,
        ];
        for line in cases {
            let findings = scan_security_patterns_in_text("CHANGELOG.md", line);
            assert!(
                findings.is_empty(),
                "markdown prose must be skipped wholesale: {line} → {findings:?}"
            );
        }
    }

    /// Vue directive binding (shorthand `:prop="…"` and long
    /// `v-bind:prop="…"`) — the LHS attribute name starts with `:`
    /// or `v-bind:`, so the bound value is a Vue expression (often a
    /// function call), not a string literal credential. The
    /// reproducer is `frontend/src/components/Config/GitPlatformForm.vue`
    /// lines 54 / 84 / 112 — every line is `:show-password="…"` —
    /// and the r1 reviewer-sec F1 extension adds the long
    /// `v-bind:` form (`v-bind:password="fn(…)"`) and the
    /// short-form identifier binding (`:api-key="identifier"`).
    /// Both are directive bindings; both must suppress.
    #[test]
    fn scan_security_patterns_in_text_skips_vue_directive_bindings() {
        let cases: &[(&str, &str)] = &[
            // M28 reproducer — `frontend/src/components/Config/GitPlatformForm.vue:54`
            (
                "frontend/src/components/Config/GitPlatformForm.vue",
                r#"              :show-password="showPasswordFor(draft.token)""#,
            ),
            // M28 reproducer — `:84`
            (
                "frontend/src/components/Config/GitPlatformForm.vue",
                r#"              :show-password="showPasswordFor(draft.webhookSigningSecret)""#,
            ),
            // M28 reproducer — `:112`
            (
                "frontend/src/components/Config/GitPlatformForm.vue",
                r#"              :show-password="showPasswordFor(draft.webhookSecret)""#,
            ),
            // r1 reviewer-sec F1 extension — long-form `v-bind:`
            // directive, expression binding.
            (
                "frontend/src/components/Config/GitPlatformForm.vue",
                r#"              v-bind:password="showPasswordFor(draft.token)""#,
            ),
            // r1 reviewer-sec F1 extension — short-form `:api-key`
            // with a JS identifier expression (no inner string
            // literal — the regex `[^'"]+` would not have matched a
            // string-literal binding anyway).
            (
                "frontend/src/components/Config/GitPlatformForm.vue",
                r#"              :api-key="someLongIdentifier1234""#,
            ),
        ];
        for (path, line) in cases {
            let findings = scan_security_patterns_in_text(path, line);
            assert!(
                findings.is_empty(),
                "Vue directive binding must be skipped: {path}: {line} → {findings:?}"
            );
        }
    }

    /// Static Vue attribute — the LHS attribute name does **not** start
    /// with `:` or `v-bind:`. The value is a string literal; it must
    /// report even when the LHS is hyphenated like `show-password`.
    /// The r1 reviewer-sec F1 counterexample:
    /// `<my-elem show-password="hunter2_real">` is a real hardcoded
    /// credential leak (no directive prefix), not a Vue expression.
    /// Pinned alongside the directive-binding test so the discriminator
    /// cannot drift: suppress the binding form, report the static form.
    #[test]
    fn scan_security_patterns_in_text_keeps_static_vue_attributes() {
        let cases: &[&str] = &[
            // r1 reviewer-sec F1 — static hyphenated name + literal
            // value: this is the over-suppressed case the r1 rule
            // must NOT swallow.
            r#"<my-elem show-password="hunter2_real">"#,
            // Same shape, `api-key` form (the api.?key pattern).
            r#"<my-elem api-key="AKIAIOSFODNN7EXAMPLE">"#,
            // Plain (non-hyphenated) static attribute + literal.
            r#"<my-elem password="hunter2_real">"#,
        ];
        for line in cases {
            let findings = scan_security_patterns_in_text("frontend/src/components/MyComponent.vue", line);
            assert!(
                !findings.is_empty(),
                "static Vue attribute with literal value must report: {line} → {findings:?}"
            );
        }
    }

    /// `//` line comments — language-agnostic; covers regular `//`
    /// (Rust / JS / TS), doc `///` and module-level `//!` (Rust), and
    /// `//` comments in `.vue` `<script>` blocks (Vue templates use
    /// HTML `<!-- -->` comments, not `//`). Python (`#`) and shell /
    /// YAML (`#`) are NOT affected — those have different comment
    /// markers. The reproducer corpus is 13 lines spread across
    /// `src/models/secrets.rs` (×9), `src/output/renderer.rs`,
    /// `src/output/team_renderer.rs`, `src/repo/experts/facts.rs`,
    /// and `src/cli/handlers/output.rs`.
    #[test]
    fn scan_security_patterns_in_text_skips_double_slash_line_comments() {
        let cases: &[&str] = &[
            // src/models/secrets.rs:5 (module-level doc comment)
            r#"//! line of `config.toml` with `password = "real_password"` is exactly the"#,
            // src/models/secrets.rs:46
            r#"//! password = "***[redacted:kind=password,len=12,fp=abcdef0123456789,key=password]***""#,
            // src/models/secrets.rs:79
            r#"    /// `password = "…"`, `db_password: …`, etc."#,
            // src/models/secrets.rs:128
            r#"/// assignment patterns (so the marker can echo `password = "…"` instead of"#,
            // src/models/secrets.rs:184
            r#"/// The order matters: structured assignments (`password = "…"`) are scanned"#,
            // src/models/secrets.rs:318
            r#"/// placeholder (`password = "x"`) than a real secret, and redacting it makes"#,
            // src/models/secrets.rs:360
            r#"/// `password = "glpat-…"` line is reported as `password`, not also as a"#,
            // src/models/secrets.rs:447
            r#"/// `key = value` lines stay syntactically intact: `password = "abc123"` is"#,
            // src/models/secrets.rs:448
            r#"/// rewritten to `password = "***[redacted:kind=password,len=6,fp=…,key=password]***"`,"#,
            // src/output/renderer.rs:156
            r#"            // password = 'hunter2_real' claim is wrong, the actual value"#,
            // src/output/team_renderer.rs:238
            r#"                // like `leaked: password = "hunter2_real_secret"` reaching"#,
            // src/repo/experts/facts.rs:178
            r#"/// / 994 — four `password: "sk"` assignments inside the file's"#,
            // src/cli/handlers/output.rs:94
            r#"/// read it, which means a quoted `password = "real_password"` line in"#,
        ];
        for line in cases {
            let findings = scan_security_patterns_in_text("src/lib.rs", line);
            assert!(
                findings.is_empty(),
                "`//` line comment must be skipped: {line} → {findings:?}"
            );
        }
    }

    /// Identifier-only `api.?key` match in a `.rs` source file:
    /// `api_key: encrypt_non_empty(…)` in a struct literal is the
    /// M28 reproducer at `src/store/rows.rs:143` — the value is a
    /// function call, not a hardcoded literal. The skip is gated by
    /// `.rs` extension + `:` separator + no quote consumed, so:
    ///
    /// - an indirect assignment `api_key = someLongVariableName123`
    ///   (separator `=`) keeps reporting — that's the existing
    ///   `finds_api_key` / `…_reports_correct_line_numbers` shape
    ///   and must not regress;
    /// - a quoted literal `api_key = "…"` must still report;
    /// - **the same shape in `.yaml` / `.toml` / `.json` keeps
    ///   reporting** — in YAML, `api_key: AKIAIOSFODNN7EXAMPLE` is
    ///   a real hardcoded credential, not a struct literal.
    ///   Pinned by `_keeps_unquoted_api_key_in_config_formats`.
    #[test]
    fn scan_security_patterns_in_text_skips_identifier_only_api_key() {
        // Struct field with function-call RHS: separator is `:` and
        // no quote consumed — all three gate conditions met, skip
        // fires.
        let line = r#"        api_key: encrypt_non_empty(&config.api_key, key)?,"#;
        let findings = scan_security_patterns_in_text("src/store/rows.rs", line);
        assert!(
            findings.is_empty(),
            "identifier-only api_key match (`.rs` + sep `:` + no quote) must be skipped: {line} → {findings:?}"
        );

        // Same shape, no space around `:` — the separator search still
        // finds `:` and the quote check still fails.
        let line = r#"        api_key:encrypt_non_empty(&config.api_key,key)?,"#;
        let findings = scan_security_patterns_in_text("src/store/rows.rs", line);
        assert!(
            findings.is_empty(),
            "tight-spaced struct-field api_key must be skipped: {line} → {findings:?}"
        );

        // Quoted struct literal — separator is `:` but a quote WAS
        // consumed, so the skip does NOT fire. This is the
        // "preserve true positives" case: a struct literal with a
        // literal credential IS a real leak.
        let real = "a".repeat(16);
        let line = format!(r#"        api_key: "{real}","#);
        let findings = scan_security_patterns_in_text("src/store/rows.rs", &line);
        assert!(
            findings.iter().any(|f| f.pattern == "Possible API key"),
            "quoted struct-literal api_key must still report: {findings:?}"
        );

        // Indirect assignment — separator is `=`, not `:`, so the
        // skip does NOT fire even though no quote was consumed.
        // This is the existing M19 / RENG-19 behaviour: the unquoted
        // 16-char token after `api_key=` is treated as a real
        // assignment shape.
        let line = r#"        api_key = someLongVariableName123;"#;
        let findings = scan_security_patterns_in_text("src/store.rs", line);
        assert!(
            !findings.is_empty(),
            "unquoted api_key assignment (sep `=`) must still report: {findings:?}"
        );
    }

    /// r1 reviewer-sec F2 — the `api.?key` skip is **narrowed to
    /// `.rs`**, so the same struct-field shape in config formats
    /// (`.yaml` / `.yml` / `.toml` / `.json`) keeps reporting. The
    /// reproducer is `config/app.yaml` with
    /// `api_key: AKIAIOSFODNN7EXAMPLE` — an unquoted YAML scalar IS
    /// a real hardcoded credential, not a Rust struct literal.
    /// Without the narrowing the M28 r1 rule swallows every
    /// unquoted `api_key:` value in any config file (false-negative
    /// direction), which the task guard line
    /// "保有真阳性（真实赋值形态）不回退" forbids.
    #[test]
    fn scan_security_patterns_in_text_keeps_unquoted_api_key_in_config_formats() {
        // YAML — unquoted `api_key:` value. The struct-field skip
        // does NOT fire (file is not `.rs`), so the regex match
        // reports.
        let yaml_line = "api_key: AKIAIOSFODNN7EXAMPLE";
        for ext in ["yaml", "yml"] {
            let path = format!("config/app.{ext}");
            let findings = scan_security_patterns_in_text(&path, yaml_line);
            assert!(
                findings.iter().any(|f| f.pattern == "Possible API key"),
                "unquoted api_key in .{ext} must still report: {findings:?}"
            );
        }

        // TOML — unquoted `api_key = value` (the TOML separator is
        // `=`; this case is already kept by the separator gate).
        // Pinned here for symmetry: TOML matches the existing
        // `api_key = "…"` shape (separator `=`), so it was never
        // affected by the r1 skip. Confirm.
        let toml_line = r#"api_key = "AKIAIOSFODNN7EXAMPLE""#;
        let findings = scan_security_patterns_in_text("config/app.toml", toml_line);
        assert!(
            findings.iter().any(|f| f.pattern == "Possible API key"),
            "quoted api_key in .toml must still report: {findings:?}"
        );

        // JSON — unquoted keys are not legal JSON, but a JSON file
        // could carry a YAML-style comment by accident; skipped by
        // the test because JSON strings require quotes around values.
        // We do not add a JSON-pin here; the YAML pins cover the
        // F2 reproducer.

        // `src/store/rows.rs` form — MUST still skip, the M28
        // reproducer. The `.rs` gate keeps the existing suppression
        // for struct-literal field initializers with function-call
        // RHS.
        let rs_line = r#"        api_key: encrypt_non_empty(&config.api_key, key)?,"#;
        let findings = scan_security_patterns_in_text("src/store/rows.rs", rs_line);
        assert!(
            findings.is_empty(),
            "src/store/rows.rs struct-field api_key must still skip: {findings:?}"
        );
    }

    /// r1 reviewer-sec F2 — YAML `#` comments are NOT `//` comments,
    /// so the line-comment skip does not fire on them. An unquoted
    /// `api_key: AKIA…` inside a YAML comment is therefore surfaced
    /// by the scan (the comment-context silence is a separate
    /// question for a future round; the M28 spec is explicit that
    /// only the `//` line-comment shape is in scope).
    #[test]
    fn scan_security_patterns_in_text_keeps_yaml_hash_comment_with_api_key() {
        let cases: &[(&str, &str)] = &[
            (
                "config/app.yaml",
                "  # api_key: AKIAIOSFODNN7EXAMPLE  # TODO: move to env",
            ),
            ("config/app.yml", "# api_key: AKIAIOSFODNN7EXAMPLE"),
        ];
        for (path, line) in cases {
            let findings = scan_security_patterns_in_text(path, line);
            assert!(
                findings.iter().any(|f| f.pattern == "Possible API key"),
                "YAML `#` comment with api_key shape must still report: {path}: {line} → {findings:?}"
            );
        }
    }

    /// Real positives must keep firing after the M28 skip layers:
    /// - production `api_key = "literal"` in a non-test `.rs` file
    ///   (quote consumed → Possible API key),
    /// - **r2**: production `api_key: "literal"` in a non-test `.rs`
    ///   file (quoted struct literal → Possible API key — the
    ///   `.rs` gating of the identifier-only skip does NOT touch
    ///   quoted struct fields),
    /// - static Vue attribute `password="literal"` (no directive
    ///   prefix → reports — the r1 hyphenated-name rule was a
    ///   coarse over-suppression; r2 narrows to directive bindings
    ///   and reports the static form),
    /// - `let password = "real_value"` in a non-test `.rs` file
    ///   (line is not a comment, file is not markdown, no Vue prop
    ///   binding, no api_key, no placeholder),
    /// - 20-char `token = "long_lit"` literal (the token regex's
    ///   20-char value floor).
    #[test]
    fn scan_security_patterns_in_text_keeps_real_positives_after_m28_skips() {
        let real = "a".repeat(16);

        // Production-line `api_key = "literal"` (separator `=`) —
        // must fire.
        let line = format!(r#"let config = Config {{ api_key = "{real}" }};"#);
        let findings = scan_security_patterns_in_text("src/lib.rs", &line);
        assert!(
            findings.iter().any(|f| f.pattern == "Possible API key"),
            "quoted api_key assignment must still report: {findings:?}"
        );

        // Production-line `api_key: "literal"` (separator `:` but
        // quote consumed) — must fire. The M28 r2 rule is gated on
        // *no quote consumed*, so the quoted struct-literal shape
        // reports.
        let line = format!(r#"let config = Config {{ api_key: "{real}" }};"#);
        let findings = scan_security_patterns_in_text("src/lib.rs", &line);
        assert!(
            findings.iter().any(|f| f.pattern == "Possible API key"),
            "quoted struct-literal api_key must still report: {findings:?}"
        );

        // r2 — static Vue attribute `password="literal"` (no
        // directive prefix) must report. This is the r1 reviewer-sec
        // F1 counterexample: the old `mat.start() - 1 == b'-'`
        // rule over-suppressed the literal case; r2 narrows to
        // directive bindings (LHS starts with `:` / `v-bind:`).
        let line = r#"<my-elem password="hunter2_real" />"#;
        let findings = scan_security_patterns_in_text("frontend/src/components/MyComponent.vue", line);
        assert!(
            findings.iter().any(|f| f.pattern == "Hardcoded password"),
            "static Vue password attribute must still report: {findings:?}"
        );

        // Production-line `let password = "real"` in a regular `.rs`
        // file (not a comment line) — must fire.
        let line = r#"    let password = "real_secret_123";"#;
        let findings = scan_security_patterns_in_text("src/lib.rs", line);
        assert!(
            findings.iter().any(|f| f.pattern == "Hardcoded password"),
            "real password assignment must still report: {findings:?}"
        );

        // Production-line `token = "long_lit"` (20+ chars) — must fire.
        let token_value = "a".repeat(20);
        let line = format!(r#"    let token = "{token_value}";"#);
        let findings = scan_security_patterns_in_text("src/lib.rs", &line);
        assert!(
            findings.iter().any(|f| f.pattern == "Possible token"),
            "real token literal must still report: {findings:?}"
        );
    }

    /// Comprehensive regression corpus: every one of the 20
    /// mechanical false positives from the M25 self-audit
    /// (`reports/reng-self-audit/raw-report.md` security section,
    /// 20 lines all marked `[mechanical]`) must now return zero
    /// findings. A single failing row is a regression — the
    /// security score would jump back to 68/100 on the same audit
    /// and the per-finding noise would return.
    ///
    /// Every corpus row is a verbatim substring of the file named
    /// in the path column. The CHANGELOG.md ×3 rows are byte-for-byte
    /// from the actual file (the r1 reviewer-sec N1 nit closed the
    /// "缩略串" gap on those rows); the other 17 rows were already
    /// byte-for-byte in r1.
    ///
    /// The corpus is parameterised by `(file, line)` so a future
    /// review can extend the list without touching the assertion
    /// shape; the assert message quotes both pieces so a failure
    /// points at the offending reproducer directly.
    #[test]
    fn scan_security_patterns_in_text_skips_m28_regression_corpus() {
        let corpus: &[(&str, &str)] = &[
            // (a) Markdown prose — verbatim CHANGELOG.md ×3 fragments.
            // CHANGELOG.md:4 — actual line contains
            // `` `password: "sk"` `` and `` `password = "sk"` ``. Per
            // `(line, pattern)` break collapses to one finding, but the
            // path-level markdown skip is content-agnostic so any
            // substring containing a regex hit also reports 0 findings.
            (
                "CHANGELOG.md",
                r#"Sirena's `crates/sirena-cli/src/upload.rs` is the reproducer: 4 `password: "sk"` assignments at lines 673 / 741 / 843 / 994"#,
            ),
            // CHANGELOG.md:13 — actual line contains `` `password = "…"` ``.
            (
                "CHANGELOG.md",
                r#"deduct `8` points per regex hit (capped at 20) for `api_key = "…"`, `password = "…"`, `sk-…` and `-----BEGIN … PRIVATE KEY-----`"#,
            ),
            // CHANGELOG.md:41 — actual line contains `` `password = "…"` ``,
            // `` `api_key = "…"` ``, `` `token = "…"` ``.
            (
                "CHANGELOG.md",
                r#"The detector recognises `password = "…"`, `api_key = "…"`, `token = "…"`, `private_key = "…"`, PEM blocks, `https://user:pass@host` URLs, `Authorization: Bearer …` headers"#,
            ),
            // (b) Vue prop bindings — GitPlatformForm.vue ×3
            (
                "frontend/src/components/Config/GitPlatformForm.vue",
                r#"              :show-password="showPasswordFor(draft.token)""#,
            ),
            (
                "frontend/src/components/Config/GitPlatformForm.vue",
                r#"              :show-password="showPasswordFor(draft.webhookSigningSecret)""#,
            ),
            (
                "frontend/src/components/Config/GitPlatformForm.vue",
                r#"              :show-password="showPasswordFor(draft.webhookSecret)""#,
            ),
            // (c) Rust line comments — secrets.rs ×9
            (
                "src/models/secrets.rs",
                r#"//! line of `config.toml` with `password = "real_password"` is exactly the"#,
            ),
            (
                "src/models/secrets.rs",
                r#"//! password = "***[redacted:kind=password,len=12,fp=abcdef0123456789,key=password]***""#,
            ),
            (
                "src/models/secrets.rs",
                r#"    /// `password = "…"`, `db_password: …`, etc."#,
            ),
            (
                "src/models/secrets.rs",
                r#"/// assignment patterns (so the marker can echo `password = "…"` instead of"#,
            ),
            (
                "src/models/secrets.rs",
                r#"/// The order matters: structured assignments (`password = "…"`) are scanned"#,
            ),
            (
                "src/models/secrets.rs",
                r#"/// placeholder (`password = "x"`) than a real secret, and redacting it makes"#,
            ),
            (
                "src/models/secrets.rs",
                r#"/// `password = "glpat-…"` line is reported as `password`, not also as a"#,
            ),
            (
                "src/models/secrets.rs",
                r#"/// `key = value` lines stay syntactically intact: `password = "abc123"` is"#,
            ),
            (
                "src/models/secrets.rs",
                r#"/// rewritten to `password = "***[redacted:kind=password,len=6,fp=…,key=password]***"`,"#,
            ),
            // (c) Rust line comments — output / renderer / team_renderer / facts / handlers ×5
            (
                "src/output/renderer.rs",
                r#"            // password = 'hunter2_real' claim is wrong, the actual value"#,
            ),
            (
                "src/output/team_renderer.rs",
                r#"                // like `leaked: password = "hunter2_real_secret"` reaching"#,
            ),
            (
                "src/repo/experts/facts.rs",
                r#"/// / 994 — four `password: "sk"` assignments inside the file's"#,
            ),
            (
                "src/cli/handlers/output.rs",
                r#"/// read it, which means a quoted `password = "real_password"` line in"#,
            ),
            // (c) Identifier-only api_key — store/rows.rs ×1
            (
                "src/store/rows.rs",
                r#"        api_key: encrypt_non_empty(&config.api_key, key)?,"#,
            ),
        ];
        assert_eq!(
            corpus.len(),
            20,
            "M28 regression corpus must list every M25 self-audit FP"
        );
        for (idx, (path, line)) in corpus.iter().enumerate() {
            let findings = scan_security_patterns_in_text(path, line);
            assert!(
                findings.is_empty(),
                "M28 FP #{idx} regressed at {path}: {line} → {findings:?}"
            );
        }
    }

    /// Belt-and-braces: the aggregated `scan_security_patterns` entry
    /// skips markdown paths before reading the file, so a CHANGELOG
    /// with the three prose bullets never even reaches the in-text
    /// scan. The two layers (entry-level + in-text) agree.
    #[test]
    fn scan_security_patterns_skips_markdown_entries() {
        let dir = tempfile::tempdir().unwrap();
        let body = r#"- **Real-looking bullet** with `password = "real_password"` to ensure the regex fires"#;

        let md_path = dir.path().join("CHANGELOG.md");
        let rs_path = dir.path().join("src").join("lib.rs");
        std::fs::create_dir_all(rs_path.parent().unwrap()).unwrap();
        std::fs::write(&md_path, body).unwrap();
        std::fs::write(&rs_path, body).unwrap();

        let entries = vec![
            FileEntry {
                path: md_path.to_string_lossy().into_owned(),
                language: "Documentation".to_string(),
                loc: 1,
                is_binary: false,
                is_generated: false,
            },
            FileEntry {
                path: rs_path.to_string_lossy().into_owned(),
                language: "Rust".to_string(),
                loc: 1,
                is_binary: false,
                is_generated: false,
            },
        ];

        let findings = scan_security_patterns(&entries);
        assert_eq!(
            findings.len(),
            1,
            "only the .rs entry should produce a finding, got {findings:?}"
        );
        assert!(
            findings[0].file.ends_with("lib.rs"),
            "the surviving hit must be the non-markdown file: {:?}",
            findings[0]
        );
    }
}
