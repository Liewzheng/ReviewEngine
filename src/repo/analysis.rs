use std::collections::HashMap;

use super::FileEntry;

/// Result of code analysis for a repository.
#[derive(Debug, Clone)]
pub struct RepoAnalysis {
    pub large_files: Vec<FileAnalysis>,
    pub language_breakdown: Vec<LanguageBreakdown>,
    pub security_patterns: Vec<SecurityFinding>,
    pub health_score: u8,
}

/// Per-file analysis result.
#[derive(Debug, Clone)]
pub struct FileAnalysis {
    pub path: String,
    pub loc: usize,
    pub language: String,
    pub issues: Vec<String>,
}

/// Language breakdown entry.
#[derive(Debug, Clone)]
pub struct LanguageBreakdown {
    pub language: String,
    pub files: usize,
    pub loc: usize,
    pub percentage: f64,
}

/// A potential security issue found via pattern matching.
#[derive(Debug, Clone)]
pub struct SecurityFinding {
    pub file: String,
    pub pattern: String,
    pub line: usize,
    pub severity: String,
}

/// Analyze the scanned file entries and produce analysis results.
pub fn analyze(entries: &[FileEntry]) -> RepoAnalysis {
    let large_files = find_large_files(entries);
    let language_breakdown = build_language_breakdown(entries);
    let security_patterns = scan_security_patterns(entries);
    let score = crate::scoring::repo::score_repository(entries, &large_files, &security_patterns);

    RepoAnalysis {
        health_score: score.health_score,
        large_files,
        language_breakdown,
        security_patterns,
    }
}

/// Find files that exceed size thresholds.
fn find_large_files(entries: &[FileEntry]) -> Vec<FileAnalysis> {
    let mut large = Vec::new();
    for entry in entries {
        if entry.is_generated || entry.is_binary {
            continue;
        }

        let threshold = if entry.language == "Documentation" { 1000 } else { 500 };

        let mut issues = Vec::new();
        if entry.loc > 1000 {
            issues.push(format!("File has {} lines, consider splitting", entry.loc));
        } else if entry.loc > threshold {
            issues.push(format!("File has {} lines, consider refactoring", entry.loc));
        }
        if !issues.is_empty() {
            large.push(FileAnalysis {
                path: entry.path.clone(),
                loc: entry.loc,
                language: entry.language.clone(),
                issues,
            });
        }
    }
    large
}

/// Build language breakdown with percentages.
fn build_language_breakdown(entries: &[FileEntry]) -> Vec<LanguageBreakdown> {
    let mut lang_map: HashMap<String, (usize, usize)> = HashMap::new();
    let total_loc: usize = entries.iter().map(|e| e.loc).sum();

    for entry in entries {
        let (files, loc) = lang_map.entry(entry.language.clone()).or_insert((0, 0));
        *files += 1;
        *loc += entry.loc;
    }

    let mut breakdown: Vec<LanguageBreakdown> = lang_map
        .into_iter()
        .map(|(language, (files, loc))| LanguageBreakdown {
            language,
            files,
            loc,
            percentage: if total_loc > 0 {
                (loc as f64 / total_loc as f64) * 100.0
            } else {
                0.0
            },
        })
        .collect();

    breakdown.sort_by_key(|b| std::cmp::Reverse(b.loc));
    breakdown
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

/// Scan a single text block for security patterns. Exposed for unit testing.
pub(crate) fn scan_security_patterns_in_text(file: &str, content: &str) -> Vec<SecurityFinding> {
    let re_list = build_security_regexes();
    let mut findings = Vec::new();

    for (i, line) in content.lines().enumerate() {
        for (re, desc) in &re_list {
            if re.is_match(line) {
                findings.push(SecurityFinding {
                    file: file.to_string(),
                    pattern: desc.to_string(),
                    line: i + 1,
                    severity: "medium".to_string(),
                });
            }
        }
    }

    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entry(path: &str, language: &str, loc: usize) -> FileEntry {
        FileEntry {
            path: path.to_string(),
            language: language.to_string(),
            loc,
            is_binary: false,
            is_generated: false,
        }
    }

    #[test]
    fn test_find_large_files() {
        let entries = [make_entry("big.rs", "Rust", 600), make_entry("small.rs", "Rust", 50)];
        let large = find_large_files(&entries);
        assert_eq!(large.len(), 1);
        assert_eq!(large[0].path, "big.rs");
    }

    #[test]
    fn test_find_large_files_skips_generated() {
        // Mark Cargo.lock as generated
        let mut generated = make_entry("Cargo.lock", "Config", 3000);
        generated.is_generated = true;
        let entries = vec![generated, make_entry("big.rs", "Rust", 600)];
        let large = find_large_files(&entries);
        assert_eq!(large.len(), 1);
        assert_eq!(large[0].path, "big.rs");
    }

    #[test]
    fn test_find_large_files_doc_threshold() {
        let entries = [
            make_entry("guide.md", "Documentation", 800),
            make_entry("big.rs", "Rust", 600),
        ];
        let large = find_large_files(&entries);
        assert_eq!(large.len(), 1);
        assert_eq!(large[0].path, "big.rs");
    }

    #[test]
    fn test_language_breakdown() {
        let entries = vec![make_entry("a.rs", "Rust", 100), make_entry("b.py", "Python", 100)];
        let breakdown = build_language_breakdown(&entries);
        assert_eq!(breakdown.len(), 2);
        assert!(breakdown[0].language == "Python" || breakdown[0].language == "Rust");
    }

    #[test]
    fn find_large_files_uses_default_threshold_of_500_lines() {
        let entries = [make_entry("exact.rs", "Rust", 500), make_entry("over.rs", "Rust", 501)];
        let large = find_large_files(&entries);
        assert_eq!(large.len(), 1);
        assert_eq!(large[0].path, "over.rs");
    }

    #[test]
    fn find_large_files_uses_documentation_threshold_of_1000_lines() {
        let entries = [
            make_entry("guide.md", "Documentation", 1000),
            make_entry("huge.md", "Documentation", 1001),
        ];
        let large = find_large_files(&entries);
        assert_eq!(large.len(), 1);
        assert_eq!(large[0].path, "huge.md");
    }

    #[test]
    fn find_large_files_skips_binary_files() {
        let mut binary = make_entry("blob.bin", "Other", 5000);
        binary.is_binary = true;
        let entries = [binary, make_entry("big.rs", "Rust", 600)];
        let large = find_large_files(&entries);
        assert_eq!(large.len(), 1);
        assert_eq!(large[0].path, "big.rs");
    }

    #[test]
    fn scan_security_patterns_in_text_finds_api_key() {
        let findings = scan_security_patterns_in_text("config.env", "api_key=abc123def456ghi789");
        assert!(!findings.is_empty());
        assert!(findings.iter().any(|f| f.pattern == "Possible API key"));
    }

    #[test]
    fn scan_security_patterns_in_text_finds_hardcoded_password() {
        let findings = scan_security_patterns_in_text("main.rs", r#"password = "supersecret123""#);
        assert!(!findings.is_empty());
        assert!(findings.iter().any(|f| f.pattern == "Hardcoded password"));
    }

    #[test]
    fn scan_security_patterns_in_text_finds_secret_key() {
        let findings = scan_security_patterns_in_text("keys.env", "sk-abcdefghijklmnopqrstuvwxyz123456");
        assert!(!findings.is_empty());
        assert!(findings.iter().any(|f| f.pattern == "Possible secret key"));
    }

    #[test]
    fn scan_security_patterns_in_text_finds_private_key() {
        let content = "-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAKCAQEA...";
        let findings = scan_security_patterns_in_text("key.pem", content);
        assert!(!findings.is_empty());
        assert!(findings.iter().any(|f| f.pattern == "Private key"));
    }

    #[test]
    fn scan_security_patterns_in_text_reports_correct_line_numbers() {
        let content = "safe\napi_key=abc123def456ghi789\nsafe again";
        let findings = scan_security_patterns_in_text("config.env", content);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].line, 2);
    }

    #[test]
    fn scan_security_patterns_in_text_returns_empty_for_clean_content() {
        let findings = scan_security_patterns_in_text("main.rs", "fn main() { println!(\"hello\"); }");
        assert!(findings.is_empty());
    }
}
