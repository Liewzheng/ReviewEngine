//! Tool command that generates code improvement suggestions by analyzing diffs with an LLM.
//!
//!
//! @module review-engine
use crate::llm::client::LLMClient;
use crate::models::*;
use crate::prompt::PromptEngine;
use anyhow::Result;

/// A code suggestion with original and improved code.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CodeSuggestion {
    pub file: String,
    pub line: u32,
    pub original_code: String,
    pub improved_code: String,
    pub suggestion: String,
    pub score: u8,
}

/// Output from the improve command.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ImproveOutput {
    pub code_suggestions: Vec<CodeSuggestion>,
}

/// Run the improve command: generate code suggestions.
pub async fn run_improve(
    llm_client: &LLMClient,
    llm_configs: &[LLMConfig],
    diff: &str,
    mr_info: &MRInfo,
) -> Result<ImproveOutput> {
    let prompt_engine = PromptEngine::new();
    let (system, user) = prompt_engine.build_improve_prompt(diff, mr_info)?;
    let result = llm_client.complete_with_fallback(llm_configs, &system, &user).await?;
    parse_improve_response(&result.content)
}

fn parse_improve_response(response: &str) -> Result<ImproveOutput> {
    let cleaned = crate::output::parser::clean_yaml(response);
    if let Ok(value) = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(&cleaned) {
        let suggestions = value["code_suggestions"]
            .as_sequence()
            .map(|seq| {
                seq.iter()
                    .map(|s| CodeSuggestion {
                        file: s["file"]
                            .as_str()
                            .unwrap_or_else(|| {
                                tracing::warn!("improve suggestion missing 'file' field");
                                ""
                            })
                            .to_string(),
                        line: s["line"].as_u64().unwrap_or_else(|| {
                            tracing::warn!("improve suggestion missing 'line' field");
                            0
                        }) as u32,
                        original_code: s["original_code"]
                            .as_str()
                            .unwrap_or_else(|| {
                                tracing::warn!("improve suggestion missing 'original_code' field");
                                ""
                            })
                            .to_string(),
                        improved_code: s["improved_code"]
                            .as_str()
                            .unwrap_or_else(|| {
                                tracing::warn!("improve suggestion missing 'improved_code' field");
                                ""
                            })
                            .to_string(),
                        suggestion: s["suggestion"]
                            .as_str()
                            .unwrap_or_else(|| {
                                tracing::warn!("improve suggestion missing 'suggestion' field");
                                ""
                            })
                            .to_string(),
                        score: s["score"].as_u64().unwrap_or_else(|| {
                            tracing::warn!("improve suggestion missing 'score' field; defaulting to 5");
                            5
                        }) as u8,
                    })
                    .collect()
            })
            .unwrap_or_else(|| {
                tracing::warn!("improve response missing 'code_suggestions' array; returning empty list");
                vec![]
            });
        return Ok(ImproveOutput {
            code_suggestions: suggestions,
        });
    }
    let excerpt: String = response.chars().take(200).collect();
    tracing::warn!("Failed to parse improve response as YAML; returning empty suggestions. Excerpt: {excerpt:?}");
    Ok(ImproveOutput {
        code_suggestions: vec![],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_improve_yaml() {
        let yaml = r#"
code_suggestions:
  - file: "src/main.rs"
    line: 42
    original_code: "let x = 1;"
    improved_code: "let x = 2;"
    suggestion: "Use a better value"
    score: 8
"#;
        let output = parse_improve_response(yaml).unwrap();
        assert_eq!(output.code_suggestions.len(), 1);
        assert_eq!(output.code_suggestions[0].file, "src/main.rs");
        assert_eq!(output.code_suggestions[0].score, 8);
    }

    #[test]
    fn test_parse_improve_malformed_yaml_returns_empty() {
        let bad = "{{invalid yaml}}: [";
        let output = parse_improve_response(bad).unwrap();
        assert!(output.code_suggestions.is_empty());
    }

    #[test]
    fn test_parse_improve_missing_suggestions_array() {
        let yaml = "some_key: some_value\n";
        let output = parse_improve_response(yaml).unwrap();
        assert!(output.code_suggestions.is_empty());
    }

    #[test]
    fn test_parse_improve_suggestion_missing_fields() {
        let yaml = r#"
code_suggestions:
  - suggestion: "Improve this"
"#;
        let output = parse_improve_response(yaml).unwrap();
        assert_eq!(output.code_suggestions.len(), 1);
        // missing fields get defaults
        assert!(output.code_suggestions[0].file.is_empty());
        assert_eq!(output.code_suggestions[0].line, 0);
        assert_eq!(output.code_suggestions[0].score, 5);
    }
}
