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
/// Mechanical hits in test files and on placeholder values are suppressed
/// before reporting — they would otherwise flood the report with fixture
/// noise (test inputs, formatting examples, illustrative values wrapped
/// in `〈…〉`) and drown the semantic security signal users actually care
/// about. The cap on mechanical contribution to the security score lives
/// in `static_experts::security`.
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
        if let Ok(content) = std::fs::read_to_string(&entry.path) {
            findings.extend(scan_security_patterns_in_text(&entry.path, &content));
        }
    }

    findings
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
/// Every hit is tagged [`FindingKind::Mechanical`]. Lines whose file path
/// looks like a test file, whose matched portion is a placeholder
/// (`〈…〉`, `<…>`, or all-`x`/`*` token after a structural prefix), or
/// whose line falls inside a `#[cfg(test)] mod … { … }` block (an
/// inline test module inside a regular source file — RENG-114
/// follow-up, sirena reproducer) are suppressed rather than reported.
pub(crate) fn scan_security_patterns_in_text(file: &str, content: &str) -> Vec<SecurityFinding> {
    let re_list = build_security_regexes();
    let mut findings = Vec::new();
    let skip_test = is_test_file_path(file);
    // Inline `#[cfg(test)] mod … { … }` blocks live inside regular
    // source files, so `is_test_file_path` cannot see them. The span
    // list is computed once per file (a brace-counting pass) and
    // every line is tested against it — the cost is O(L + B·K)
    // where L is the line count, B is the block count and K is the
    // average block length; for sirena's `upload.rs` (1257 lines, 1
    // block) it is one comparison per regex-bearing line.
    let cfg_test_spans = cfg_test_line_spans(content);

    for (i, line) in content.lines().enumerate() {
        if skip_test {
            // Test files are skipped wholesale: every line of a path
            // recognised by `is_test_file` is exempt, regardless of
            // what it carries. The fixture may well live next to a
            // `README.md` or a sample `.env`, but a config-shaped
            // value on a "test-blessed" path is still a fixture by
            // our rule of thumb and not a real secret. The same
            // exclusion is enforced at the entry level by
            // `scan_security_patterns`, so a file the regex never
            // reaches is also never inspected line by line — the two
            // layers are belt-and-braces for the same reason.
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

    /// RENG-114 follow-up (sirena): four `password: "sk"` assignments
    /// inside a regular `.rs` file's inline `#[cfg(test)] mod tests
    /// { … }` block must NOT produce any finding. The file is
    /// production-named (not a test file by path) and the regex hits
    /// are inside the test module — same shape as the four lines the
    /// sirena repro hit (`crates/sirena-cli/src/upload.rs` 673 / 741
    /// / 843 / 994). Without this pin the security scan would emit 4
    /// `[mechanical] Hardcoded password` findings, dragging the
    /// security score down by 16 points on a repo that has no real
    /// leak. The pin asserts **zero** findings in the cfg-test
    /// block; production lines that carry the same payload still
    /// flag (pinned by `…_keeps_production_lines_outside_cfg_test`).
    #[test]
    fn scan_security_patterns_in_text_skips_cfg_test_blocks() {
        // Mimic the sirena shape: 4 fixture `password = "sk"`
        // assignments inside an inline `#[cfg(test)] mod tests { … }`,
        // with no `password` / `api_key` / `token` pattern on any
        // production line — so the only regex candidates are inside
        // the cfg-test block, and a clean result proves the skip.
        let content = "fn real() { let config_name = \"upload\"; }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn creds() {\n        let password = \"sk\";\n    }\n    #[test]\n    fn another() {\n        let p: &str = \"sk\";\n    }\n    #[test]\n    fn third() {\n        let _ = \"sk\";\n    }\n    #[test]\n    fn fourth() {\n        let _ = \"sk\";\n    }\n}\n";
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
}
