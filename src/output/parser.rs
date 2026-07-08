use crate::models::{AggregatedReport, DiffHunk, Effort, ExpertReport, Finding, Severity};
use crate::output::renderer;
use anyhow::Result;
use regex::Regex;
use std::sync::OnceLock;

/// Maximum LLM response size in bytes (10 MiB) to prevent memory DoS from oversized YAML.
const MAX_YAML_SIZE: usize = 10 * 1024 * 1024;

/// Parse an LLM response (YAML inside optional fenced code blocks) into an [`ExpertReport`].
///
/// The parser attempts strict YAML deserialisation first. If that fails,
/// it falls back to extracting the first fenced YAML block. On complete
/// failure, it returns a best-effort report with empty findings so the
/// expert is not lost.
/// Rejects input larger than 10 MiB to prevent memory exhaustion.
pub fn parse_llm_response(expert_name: &str, yaml_text: &str) -> ExpertReport {
    if yaml_text.len() > MAX_YAML_SIZE {
        tracing::warn!("LLM response exceeds {} bytes, using fallback report", MAX_YAML_SIZE);
        return fallback_report(expert_name, yaml_text);
    }

    let cleaned = clean_yaml(yaml_text);

    match serde_yaml_ng::from_str::<serde_yaml_ng::Value>(&cleaned) {
        Ok(value) => match build_expert_report(expert_name, yaml_text, &value) {
            Ok(report) => report,
            Err(build_err) => {
                tracing::warn!(
                    expert_name = expert_name,
                    error = %build_err,
                    "Failed to build expert report from parsed YAML; using fallback"
                );
                fallback_report(expert_name, yaml_text)
            }
        },
        Err(parse_err) => {
            tracing::warn!(
                expert_name = expert_name,
                error = %parse_err,
                "Failed to parse YAML LLM response; attempting fallback extraction"
            );

            // Fallback: try to parse the first fenced YAML block in isolation.
            if let Some(fallback) = extract_first_fenced_yaml(yaml_text) {
                if let Ok(value) = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(&fallback) {
                    if let Ok(report) = build_expert_report(expert_name, yaml_text, &value) {
                        return report;
                    }
                }
            }

            fallback_report(expert_name, yaml_text)
        }
    }
}

/// Build a best-effort report with empty findings so the expert is not lost entirely.
fn fallback_report(expert_name: &str, yaml_text: &str) -> ExpertReport {
    let findings = Vec::new();
    let markdown = renderer::render_expert_markdown(expert_name, &findings);
    ExpertReport {
        expert_name: expert_name.to_string(),
        findings,
        markdown,
        raw_llm_response: yaml_text.to_string(),
    }
}

/// Parse the aggregator expert's YAML response into an [`AggregatedReport`].
///
/// Cleans the YAML (strips fences), then extracts findings and renders
/// them as aggregated Markdown. Implements a three-layer fallback:
/// 1. Strict YAML parsing; 2. Extract fenced YAML block; 3. Return empty
/// report so the pipeline does not abort.
/// Rejects input larger than 10 MiB to prevent memory exhaustion.
pub fn parse_aggregator_response(yaml_text: &str) -> Result<AggregatedReport> {
    if yaml_text.len() > MAX_YAML_SIZE {
        tracing::warn!(
            "Aggregator response exceeds {} bytes, returning empty report",
            MAX_YAML_SIZE
        );
        return Ok(AggregatedReport {
            findings: vec![],
            markdown: String::new(),
            raw_llm_response: yaml_text.to_string(),
        });
    }

    let cleaned = clean_yaml(yaml_text);

    // Layer 1: strict YAML parsing
    let value = match serde_yaml_ng::from_str::<serde_yaml_ng::Value>(&cleaned) {
        Ok(v) => {
            // If the parsed value is not a mapping (e.g. a bare string),
            // treat it as a parse failure and fall back.
            if !v.is_mapping() {
                tracing::warn!("Aggregator response parsed as scalar, not a mapping. Returning empty report.");
                return Ok(AggregatedReport {
                    findings: vec![],
                    markdown: String::new(),
                    raw_llm_response: yaml_text.to_string(),
                });
            }
            v
        }
        Err(e) => {
            tracing::warn!("Aggregator YAML parse failed: {}. Attempting fenced fallback.", e);
            // Layer 2: extract fenced YAML block
            if let Some(fallback) = extract_first_fenced_yaml(yaml_text) {
                match serde_yaml_ng::from_str::<serde_yaml_ng::Value>(&fallback) {
                    Ok(v) => v,
                    Err(e2) => {
                        tracing::warn!(
                            "Aggregator fenced YAML fallback also failed: {}. Returning empty report.",
                            e2
                        );
                        // Layer 3: empty report
                        return Ok(AggregatedReport {
                            findings: vec![],
                            markdown: String::new(),
                            raw_llm_response: yaml_text.to_string(),
                        });
                    }
                }
            } else {
                tracing::warn!("No fenced YAML block found in aggregator response. Returning empty report.");
                return Ok(AggregatedReport {
                    findings: vec![],
                    markdown: String::new(),
                    raw_llm_response: yaml_text.to_string(),
                });
            }
        }
    };

    let findings = extract_findings(&value, "aggregator").unwrap_or_default();
    let markdown = renderer::render_aggregated_markdown(&findings);

    Ok(AggregatedReport {
        findings,
        markdown,
        raw_llm_response: yaml_text.to_string(),
    })
}

fn build_expert_report(expert_name: &str, raw_response: &str, value: &serde_yaml_ng::Value) -> Result<ExpertReport> {
    let findings = extract_findings(value, expert_name)?;
    let markdown = renderer::render_expert_markdown(expert_name, &findings);

    Ok(ExpertReport {
        expert_name: expert_name.to_string(),
        findings,
        markdown,
        raw_llm_response: raw_response.to_string(),
    })
}

#[allow(clippy::unwrap_used)]
fn fence_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^```(?:yaml|YAML)?\s*$").unwrap())
}

/// Strip YAML code-fence markers from an LLM response so the remaining text
/// can be parsed as plain YAML.
pub(crate) fn clean_yaml(text: &str) -> String {
    let mut cleaned = String::new();
    let mut in_block = false;

    for line in text.lines() {
        let trimmed = line.trim();
        if fence_regex().is_match(trimmed) {
            in_block = !in_block;
            continue;
        }
        if in_block {
            cleaned.push_str(line);
            cleaned.push('\n');
        }
    }

    if cleaned.is_empty() {
        text.to_string()
    } else {
        cleaned
    }
}

#[allow(clippy::unwrap_used)]
fn first_fenced_yaml_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"```(?:yaml|YAML)?\r?\n([\s\S]*?)\r?\n```").unwrap())
}

fn extract_first_fenced_yaml(text: &str) -> Option<String> {
    first_fenced_yaml_regex()
        .captures(text)
        .and_then(|caps| caps.get(1).map(|m| m.as_str().to_string()))
}

/// Validate that findings point to files and lines present in the diff.
///
/// - Findings whose `file` is not in `diff_files` are dropped.
/// - Findings with `line: None` are kept if the file exists in the diff.
/// - Findings with a line value are kept only when that line falls within any
///   of the file's diff hunks (using the new-file / hunk range).
pub fn validate_findings(findings: &[Finding], diff_files: &[(String, Vec<DiffHunk>)]) -> Vec<Finding> {
    let diff_map: std::collections::HashMap<_, _> = diff_files.iter().map(|(p, h)| (p.as_str(), h)).collect();

    findings
        .iter()
        .filter(|f| {
            let Some(hunks) = diff_map.get(f.file.as_str()) else {
                return false;
            };
            match f.line {
                None => true,
                Some(line) => hunks.iter().any(|h| {
                    let start = h.new_start;
                    let end = h.new_start.saturating_add(h.new_lines.saturating_sub(1));
                    line >= start && line <= end
                }),
            }
        })
        .cloned()
        .collect()
}

fn extract_findings(value: &serde_yaml_ng::Value, expert_name: &str) -> Result<Vec<Finding>> {
    let mut findings = Vec::new();

    if let Some(review) = value.get("review") {
        if let Some(issues) = review.get("findings").and_then(|v| v.as_sequence()) {
            for issue in issues {
                findings.push(Finding {
                    file: issue.get("file").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    line: issue.get("line").and_then(|v| v.as_u64()).map(|v| v as u32),
                    line_end: issue.get("line_end").and_then(|v| v.as_u64()).map(|v| v as u32),
                    severity: match issue.get("severity").and_then(|v| v.as_str()).unwrap_or("medium") {
                        "critical" => Severity::Critical,
                        "high" => Severity::High,
                        "medium" => Severity::Medium,
                        "low" => Severity::Low,
                        "note" => Severity::Note,
                        _ => Severity::Medium,
                    },
                    confidence: issue
                        .get("confidence")
                        .and_then(|v| v.as_u64())
                        .map(|v| v as u8)
                        .unwrap_or(5),
                    category: issue.get("category").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    title: issue.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    summary: issue
                        .get("detail")
                        .or_else(|| issue.get("summary"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    evidence: issue.get("evidence").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    impact: issue.get("impact").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    recommendation: issue
                        .get("recommendation")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    effort: match issue.get("effort").and_then(|v| v.as_str()).unwrap_or("small") {
                        "trivial" => Effort::Trivial,
                        "small" => Effort::Small,
                        "medium" => Effort::Medium,
                        "large" => Effort::Large,
                        _ => Effort::Small,
                    },
                    expert_name: expert_name.to_string(),
                    expert_role: issue
                        .get("expert_role")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    agrees_with: vec![],
                    references: vec![],
                });
            }
        }
    }

    Ok(findings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_yaml_findings() {
        let yaml = "```yaml\n\
                     review:\n  \
                       findings:\n    \
                         - file: \"src/main.rs\"\n      \
                           line: 42\n      \
                           severity: \"high\"\n      \
                           title: \"Test issue\"\n      \
                           detail: \"Description\"\n```";
        let report = parse_llm_response("test", yaml);
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].file, "src/main.rs");
        assert_eq!(report.findings[0].severity, Severity::High);
    }

    #[test]
    fn test_clean_yaml_strips_fence() {
        let input = "```yaml\nfoo: bar\n```";
        let cleaned = clean_yaml(input);
        assert_eq!(cleaned, "foo: bar\n");
    }

    #[test]
    fn test_clean_yaml_uppercase_fence() {
        let input = "```YAML\nreview:\n  findings: []\n```\nTrailing text after the fence.";
        let cleaned = clean_yaml(input);
        assert_eq!(cleaned, "review:\n  findings: []\n");

        let report = parse_llm_response("uppercase", input);
        assert!(report.findings.is_empty());
        assert!(report.raw_llm_response.contains("```YAML"));
    }

    #[test]
    fn test_clean_yaml_plain_fence() {
        let input = "```\nfoo: bar\n```\nMore text.";
        let cleaned = clean_yaml(input);
        assert_eq!(cleaned, "foo: bar\n");
    }

    #[test]
    fn test_parse_malformed_yaml_fallback_to_empty_report() {
        let yaml = r#"
```yaml
review:
  findings:
    - file: "src/main.rs"
      line: 42
      severity: "high"
      title: "Unclosed string
      detail: "This string never ends
```
"#;
        let report = parse_llm_response("performance", yaml);
        assert!(report.findings.is_empty());
        assert!(!report.raw_llm_response.is_empty());
    }

    #[test]
    fn test_extract_findings_detail_fallback() {
        let yaml = r#"
```yaml
review:
  findings:
    - file: "src/main.rs"
      line: 42
      severity: "high"
      title: "Missing error handling"
      detail: "This function does not handle the error case"
```
"#;
        let report = parse_llm_response("quality", yaml);
        assert_eq!(report.findings.len(), 1);
        assert_eq!(
            report.findings[0].summary,
            "This function does not handle the error case"
        );
        assert_eq!(report.findings[0].expert_name, "quality");
    }

    #[test]
    fn test_extract_findings_empty_list() {
        let yaml = r#"
```yaml
review:
  findings: []
```
"#;
        let report = parse_llm_response("lead", yaml);
        assert!(report.findings.is_empty());
    }

    #[test]
    fn test_extract_findings_new_fields() {
        let yaml = r#"
```yaml
review:
  findings:
    - file: "src/lib.rs"
      line: 10
      line_end: 20
      severity: "critical"
      confidence: 9
      category: "security"
      title: "SQL Injection"
      detail: "User input is directly concatenated into SQL query"
      evidence: "let query = format!(\"SELECT * FROM users WHERE id = {}\", user_input);"
      impact: "An attacker can extract arbitrary data from the database"
      recommendation: "Use parameterized queries"
      effort: "medium"
      expert_role: "Security Lead"
```
"#;
        let report = parse_llm_response("security", yaml);
        assert_eq!(report.findings.len(), 1);
        let f = &report.findings[0];
        assert_eq!(f.file, "src/lib.rs");
        assert_eq!(f.line, Some(10));
        assert_eq!(f.line_end, Some(20));
        assert_eq!(f.severity, Severity::Critical);
        assert_eq!(f.confidence, 9);
        assert_eq!(f.category, "security");
        assert_eq!(f.title, "SQL Injection");
        assert_eq!(f.summary, "User input is directly concatenated into SQL query");
        assert!(f.evidence.contains("user_input"));
        assert!(f.impact.contains("attacker"));
        assert_eq!(f.recommendation, "Use parameterized queries");
        assert_eq!(f.effort, Effort::Medium);
        assert_eq!(f.expert_name, "security");
        assert_eq!(f.expert_role, "Security Lead");
    }

    #[test]
    fn test_clean_yaml_mixed_content() {
        let input = "Here is some intro text.\n\
                     ```yaml\n\
                     review:\n  \
                       findings:\n    \
                         - file: \"src/main.rs\"\n      \
                           line: 42\n      \
                           severity: \"high\"\n      \
                           title: \"Mixed issue\"\n      \
                           detail: \"Found in mixed content\"\n\
                     ```\n\
                     Some text after the fence.\n\
                     More trailing content.";
        let cleaned = clean_yaml(input);
        let expected = "review:\n  findings:\n    - file: \"src/main.rs\"\n      line: 42\n      severity: \"high\"\n      title: \"Mixed issue\"\n      detail: \"Found in mixed content\"\n";
        assert_eq!(cleaned, expected);
    }

    #[test]
    fn test_parse_llm_response_mixed_content() {
        let input = "Intro text before the YAML block.\n\
                     ```yaml\n\
                     review:\n  \
                       findings:\n    \
                         - file: \"src/parser.rs\"\n      \
                           line: 7\n      \
                           severity: \"medium\"\n      \
                           title: \"Parse issue\"\n      \
                           detail: \"Mixed content parse\"\n\
                     ```\n\
                     Text after the YAML block.";
        let report = parse_llm_response("quality", input);
        assert_eq!(report.findings.len(), 1);
        let f = &report.findings[0];
        assert_eq!(f.file, "src/parser.rs");
        assert_eq!(f.line, Some(7));
        assert_eq!(f.severity, Severity::Medium);
        assert_eq!(f.title, "Parse issue");
        assert_eq!(f.summary, "Mixed content parse");
        assert_eq!(f.expert_name, "quality");
    }

    #[test]
    fn parse_llm_response_parses_valid_yaml_without_fence() {
        let yaml = r#"
review:
  findings:
    - file: "src/main.rs"
      line: 1
      severity: "low"
      title: "Style"
      detail: "Missing newline"
"#;
        let report = parse_llm_response("style", yaml);
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].severity, Severity::Low);
    }

    #[test]
    fn parse_llm_response_parses_valid_json_content() {
        let json = r#"```yaml
{
  "review": {
    "findings": [
      {
        "file": "src/main.rs",
        "line": 10,
        "severity": "high",
        "title": "JSON issue",
        "detail": "Found via JSON"
      }
    ]
  }
}
```"#;
        let report = parse_llm_response("json", json);
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].file, "src/main.rs");
        assert_eq!(report.findings[0].severity, Severity::High);
    }

    #[test]
    fn parse_aggregator_response_returns_report_for_valid_yaml() {
        let yaml = r#"
```yaml
review:
  findings:
    - file: "src/lib.rs"
      line: 5
      severity: "critical"
      title: "Race condition"
      detail: "Shared state is unsynchronized"
```
"#;
        let report = parse_aggregator_response(yaml).unwrap();
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].severity, Severity::Critical);
    }

    #[test]
    fn parse_aggregator_response_returns_empty_report_for_malformed_content() {
        let yaml = "review:\n  findings: [\n    not a valid sequence";
        let report = parse_aggregator_response(yaml).unwrap();
        assert!(report.findings.is_empty());
        assert_eq!(report.markdown, "");
    }

    #[test]
    fn parse_aggregator_response_fallback_to_fenced_yaml() {
        let yaml = r#"
Some intro text.
```yaml
review:
  findings:
    - file: "src/lib.rs"
      line: 5
      severity: "critical"
      title: "Race condition"
      detail: "Shared state is unsynchronized"
```
Trailing text.
"#;
        let report = parse_aggregator_response(yaml).unwrap();
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].severity, Severity::Critical);
    }

    #[test]
    fn parse_aggregator_response_fallback_no_fenced_block() {
        let yaml = "This is not YAML at all, just plain text.";
        let report = parse_aggregator_response(yaml).unwrap();
        assert!(report.findings.is_empty());
        assert_eq!(report.markdown, "");
        assert_eq!(report.raw_llm_response, yaml);
    }

    #[test]
    fn parse_aggregator_response_fenced_fallback_also_fails() {
        let yaml = r#"
```yaml
review:
  findings: [
    invalid unclosed
```
"#;
        let report = parse_aggregator_response(yaml).unwrap();
        assert!(report.findings.is_empty());
    }

    #[test]
    fn parse_llm_response_graceful_fallback_for_broken_yaml() {
        let yaml = "findings:\n  - file: [unclosed string";
        let report = parse_llm_response("broken", yaml);
        assert!(report.findings.is_empty());
        assert!(!report.raw_llm_response.is_empty());
    }

    #[test]
    fn parse_llm_response_graceful_fallback_for_completely_invalid() {
        let yaml = "!!! not yaml !!!";
        let report = parse_llm_response("invalid", yaml);
        assert!(report.findings.is_empty());
        assert_eq!(report.markdown, "## Invalid Review\n\nNo issues found.\n");
    }

    #[test]
    fn test_clean_yaml_no_fences_returns_original() {
        let input = "review:\n  findings: []";
        let cleaned = clean_yaml(input);
        assert_eq!(cleaned, input);
    }

    #[test]
    fn test_extract_first_fenced_yaml_multiple_blocks() {
        let input = "```yaml\nfirst: block\n```\n\n```yaml\nsecond: block\n```";
        let extracted = extract_first_fenced_yaml(input).unwrap();
        assert_eq!(extracted, "first: block");
    }

    #[test]
    fn parse_aggregator_response_size_limit() {
        let huge = "x".repeat(11 * 1024 * 1024);
        let report = parse_aggregator_response(&huge).unwrap();
        assert!(report.findings.is_empty());
        assert_eq!(report.markdown, "");
        assert_eq!(report.raw_llm_response, huge);
    }

    #[test]
    fn parse_llm_response_size_limit() {
        let huge = "x".repeat(11 * 1024 * 1024);
        let report = parse_llm_response("test", &huge);
        assert!(report.findings.is_empty());
        assert_eq!(report.raw_llm_response, huge);
    }

    #[test]
    fn parse_aggregator_response_fenced_fallback_with_valid_inner_yaml() {
        let yaml = r#"
Some explanation here.
```yaml
review:
  findings:
    - file: "src/main.rs"
      line: 10
      severity: "high"
      title: "Fallback finding"
      detail: "Found in fallback block"
```
More text.
"#;
        let report = parse_aggregator_response(yaml).unwrap();
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].file, "src/main.rs");
        assert_eq!(report.findings[0].title, "Fallback finding");
    }

    #[test]
    fn validate_findings_keeps_file_without_line() {
        let findings = vec![Finding {
            file: "src/main.rs".to_string(),
            line: None,
            line_end: None,
            severity: Severity::High,
            confidence: 8,
            category: "test".to_string(),
            title: "No line".to_string(),
            summary: "summary".to_string(),
            evidence: "evidence".to_string(),
            impact: "impact".to_string(),
            recommendation: "rec".to_string(),
            effort: Effort::Small,
            expert_name: "expert".to_string(),
            expert_role: "role".to_string(),
            agrees_with: vec![],
            references: vec![],
        }];
        let diff_files = vec![("src/main.rs".to_string(), vec![])];
        let validated = validate_findings(&findings, &diff_files);
        assert_eq!(validated.len(), 1);
    }

    #[test]
    fn validate_findings_drops_file_not_in_diff() {
        let findings = vec![Finding {
            file: "src/other.rs".to_string(),
            line: Some(10),
            line_end: None,
            severity: Severity::High,
            confidence: 8,
            category: "test".to_string(),
            title: "Other file".to_string(),
            summary: "summary".to_string(),
            evidence: "evidence".to_string(),
            impact: "impact".to_string(),
            recommendation: "rec".to_string(),
            effort: Effort::Small,
            expert_name: "expert".to_string(),
            expert_role: "role".to_string(),
            agrees_with: vec![],
            references: vec![],
        }];
        let diff_files = vec![(
            "src/main.rs".to_string(),
            vec![DiffHunk {
                header: "@@ -1,5 +1,5 @@".to_string(),
                old_start: 1,
                old_lines: 5,
                new_start: 1,
                new_lines: 5,
                lines: vec![],
            }],
        )];
        let validated = validate_findings(&findings, &diff_files);
        assert!(validated.is_empty());
    }

    #[test]
    fn validate_findings_keeps_line_inside_hunk_range() {
        let findings = vec![Finding {
            file: "src/main.rs".to_string(),
            line: Some(12),
            line_end: None,
            severity: Severity::High,
            confidence: 8,
            category: "test".to_string(),
            title: "In range".to_string(),
            summary: "summary".to_string(),
            evidence: "evidence".to_string(),
            impact: "impact".to_string(),
            recommendation: "rec".to_string(),
            effort: Effort::Small,
            expert_name: "expert".to_string(),
            expert_role: "role".to_string(),
            agrees_with: vec![],
            references: vec![],
        }];
        let diff_files = vec![(
            "src/main.rs".to_string(),
            vec![DiffHunk {
                header: "@@ -10,5 +10,8 @@".to_string(),
                old_start: 10,
                old_lines: 5,
                new_start: 10,
                new_lines: 8,
                lines: vec![],
            }],
        )];
        let validated = validate_findings(&findings, &diff_files);
        assert_eq!(validated.len(), 1);
    }

    #[test]
    fn validate_findings_drops_line_outside_hunk_range() {
        let findings = vec![Finding {
            file: "src/main.rs".to_string(),
            line: Some(25),
            line_end: None,
            severity: Severity::High,
            confidence: 8,
            category: "test".to_string(),
            title: "Out of range".to_string(),
            summary: "summary".to_string(),
            evidence: "evidence".to_string(),
            impact: "impact".to_string(),
            recommendation: "rec".to_string(),
            effort: Effort::Small,
            expert_name: "expert".to_string(),
            expert_role: "role".to_string(),
            agrees_with: vec![],
            references: vec![],
        }];
        let diff_files = vec![(
            "src/main.rs".to_string(),
            vec![DiffHunk {
                header: "@@ -10,5 +10,8 @@".to_string(),
                old_start: 10,
                old_lines: 5,
                new_start: 10,
                new_lines: 8,
                lines: vec![],
            }],
        )];
        let validated = validate_findings(&findings, &diff_files);
        assert!(validated.is_empty());
    }

    #[test]
    fn validate_findings_checks_any_hunk_for_file() {
        let findings = vec![Finding {
            file: "src/main.rs".to_string(),
            line: Some(35),
            line_end: None,
            severity: Severity::High,
            confidence: 8,
            category: "test".to_string(),
            title: "Second hunk".to_string(),
            summary: "summary".to_string(),
            evidence: "evidence".to_string(),
            impact: "impact".to_string(),
            recommendation: "rec".to_string(),
            effort: Effort::Small,
            expert_name: "expert".to_string(),
            expert_role: "role".to_string(),
            agrees_with: vec![],
            references: vec![],
        }];
        let diff_files = vec![(
            "src/main.rs".to_string(),
            vec![
                DiffHunk {
                    header: "@@ -10,5 +10,5 @@".to_string(),
                    old_start: 10,
                    old_lines: 5,
                    new_start: 10,
                    new_lines: 5,
                    lines: vec![],
                },
                DiffHunk {
                    header: "@@ -30,5 +30,10 @@".to_string(),
                    old_start: 30,
                    old_lines: 5,
                    new_start: 30,
                    new_lines: 10,
                    lines: vec![],
                },
            ],
        )];
        let validated = validate_findings(&findings, &diff_files);
        assert_eq!(validated.len(), 1);
    }
}
