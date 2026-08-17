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
    assert!(
        report.parse_error.is_none(),
        "successful parse must not carry parse_error"
    );
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
    assert!(
        report.parse_error.is_some(),
        "a failed parse must surface parse_error, not silently read as 'no issues'"
    );
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
