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
                        file: s["file"].as_str().unwrap_or("").to_string(),
                        line: s["line"].as_u64().unwrap_or(0) as u32,
                        original_code: s["original_code"].as_str().unwrap_or("").to_string(),
                        improved_code: s["improved_code"].as_str().unwrap_or("").to_string(),
                        suggestion: s["suggestion"].as_str().unwrap_or("").to_string(),
                        score: s["score"].as_u64().unwrap_or(5) as u8,
                    })
                    .collect()
            })
            .unwrap_or_default();
        return Ok(ImproveOutput {
            code_suggestions: suggestions,
        });
    }
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
}
