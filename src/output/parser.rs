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
        // Never silently present a failed parse as "no issues found": carry the
        // failure so the report can surface a ⚠️ instead of a false clean bill.
        parse_error: Some("LLM response could not be parsed into a valid review; treated as no findings".to_string()),
        raw_dump_path: None,
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
            parse_error: Some("aggregator LLM response could not be parsed; treated as empty".to_string()),
            raw_dump_path: None,
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
                    parse_error: Some("aggregator LLM response could not be parsed; treated as empty".to_string()),
                    raw_dump_path: None,
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
                            parse_error: Some(
                                "aggregator LLM response could not be parsed; treated as empty".to_string(),
                            ),
                            raw_dump_path: None,
                        });
                    }
                }
            } else {
                tracing::warn!("No fenced YAML block found in aggregator response. Returning empty report.");
                return Ok(AggregatedReport {
                    findings: vec![],
                    markdown: String::new(),
                    raw_llm_response: yaml_text.to_string(),
                    parse_error: Some("aggregator LLM response could not be parsed; treated as empty".to_string()),
                    raw_dump_path: None,
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
        parse_error: None,
        raw_dump_path: None,
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
        parse_error: None,
        raw_dump_path: None,
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

pub(crate) fn extract_first_fenced_yaml(text: &str) -> Option<String> {
    first_fenced_yaml_regex()
        .captures(text)
        .and_then(|caps| caps.get(1).map(|m| m.as_str().to_string()))
}

/// Note appended to findings whose file is in the diff but whose line falls
/// outside every changed hunk. The finding is kept (downgraded) rather than
/// dropped — LLMs commonly flag the enclosing function declaration for C-like
/// languages, and a plausible finding is more useful than a silent miss.
const HUNK_OUTSIDE_NOTE: &str = "line outside diff hunk — 该行不在本次变更的 hunk 范围内，保留供参考";

/// Validate that findings point to files present in the diff.
///
/// - Findings whose `file` is NOT in `diff_files` are dropped (the original
///   anti-hallucination intent: never report on files the review never saw).
/// - Findings with `line: None` are kept when the file exists in the diff.
/// - Findings with a line value are kept when the file is in the diff; if the
///   line lies outside every changed hunk (e.g. the LLM flagged the enclosing
///   function, or the hunk is a pure deletion), the finding is **downgraded to
///   keep-with-note** instead of being dropped. `line_end` may span across
///   hunks — only the starting line must be inside a hunk for a clean keep.
pub fn validate_findings(findings: &[Finding], diff_files: &[(String, Vec<DiffHunk>)]) -> Vec<Finding> {
    let diff_map: std::collections::HashMap<_, _> = diff_files.iter().map(|(p, h)| (p.as_str(), h)).collect();

    findings
        .iter()
        .filter_map(|f| {
            let Some(hunks) = diff_map.get(f.file.as_str()) else {
                return None;
            };
            match f.line {
                None => Some(f.clone()),
                Some(line) => {
                    let in_hunk = hunks.iter().any(|h| {
                        if h.new_lines == 0 {
                            return false;
                        }
                        let start = h.new_start;
                        let end = h.new_start.saturating_add(h.new_lines.saturating_sub(1));
                        line >= start && line <= end
                    });
                    if in_hunk {
                        Some(f.clone())
                    } else {
                        let mut kept = f.clone();
                        kept.summary = if kept.summary.is_empty() {
                            format!("⚠️ {HUNK_OUTSIDE_NOTE}")
                        } else {
                            format!("{}\n\n> ⚠️ {HUNK_OUTSIDE_NOTE}", kept.summary)
                        };
                        Some(kept)
                    }
                }
            }
        })
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
mod tests;

#[cfg(test)]
mod findings_tests;
