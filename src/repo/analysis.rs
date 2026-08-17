use super::FileEntry;

/// A potential security issue found via pattern matching.
#[derive(Debug, Clone)]
pub struct SecurityFinding {
    pub file: String,
    pub pattern: String,
    pub line: usize,
    pub severity: String,
}

/// Scan files for common security patterns (API keys, passwords, etc).
pub fn scan_security_patterns(entries: &[FileEntry]) -> Vec<SecurityFinding> {
    let mut findings = Vec::new();

    for entry in entries {
        if entry.is_binary || entry.is_generated {
            continue;
        }
        if let Ok(content) = std::fs::read_to_string(&entry.path) {
            findings.extend(scan_security_patterns_in_text(&entry.path, &content));
        }
    }

    findings
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

/// Scan a single text block for security patterns. Exposed for unit testing.
pub(crate) fn scan_security_patterns_in_text(file: &str, content: &str) -> Vec<SecurityFinding> {
    let re_list = build_security_regexes();
    let mut findings = Vec::new();

    for (i, line) in content.lines().enumerate() {
        for (re, desc) in &re_list {
            for mat in re.find_iter(line) {
                if is_placeholder_token(mat.as_str()) {
                    continue; // example/dummy placeholder, not a real secret
                }
                findings.push(SecurityFinding {
                    file: file.to_string(),
                    pattern: desc.to_string(),
                    line: i + 1,
                    severity: "medium".to_string(),
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
}
