use crate::llm::client::LLMClient;
use crate::models::*;
use crate::prompt::PromptEngine;
use anyhow::Result;

/// Output from the describe command.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DescribeOutput {
    pub title: String,
    pub description: String,
    pub change_type: String,
    pub files_walkthrough: Vec<FileWalkthrough>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FileWalkthrough {
    pub file: String,
    pub summary: String,
}

/// Run the describe command: generate PR title, description, and file walkthrough.
pub async fn run_describe(
    llm_client: &LLMClient,
    llm_configs: &[LLMConfig],
    diff: &str,
    mr_info: &MRInfo,
    commit_messages: &[String],
) -> Result<DescribeOutput> {
    let prompt_engine = PromptEngine::new();
    let (system, user) = prompt_engine.build_describe_prompt(diff, mr_info, commit_messages)?;
    let result = llm_client.complete_with_fallback(llm_configs, &system, &user).await?;
    parse_describe_response(&result.content)
}

fn parse_describe_response(response: &str) -> Result<DescribeOutput> {
    // Try to extract YAML/JSON from the response
    let cleaned = crate::output::parser::clean_yaml(response);
    if let Ok(value) = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(&cleaned) {
        let title = value["title"].as_str().unwrap_or("").to_string();
        let description = value["description"].as_str().unwrap_or("").to_string();
        let change_type = value["type"].as_str().unwrap_or("refactor").to_string();
        let files = value["files"]
            .as_sequence()
            .map(|seq| {
                seq.iter()
                    .map(|f| FileWalkthrough {
                        file: f["file"].as_str().unwrap_or("").to_string(),
                        summary: f["summary"].as_str().unwrap_or("").to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        return Ok(DescribeOutput {
            title,
            description,
            change_type,
            files_walkthrough: files,
        });
    }
    // Fallback: return raw response as description
    Ok(DescribeOutput {
        title: String::new(),
        description: response.to_string(),
        change_type: "refactor".to_string(),
        files_walkthrough: vec![],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_describe_yaml() {
        let yaml = r#"
title: "Fix login bug"
description: "Fixed the authentication token expiry issue"
type: "fix"
files:
  - file: "src/auth.rs"
    summary: "Updated token refresh logic"
"#;
        let output = parse_describe_response(yaml).unwrap();
        assert_eq!(output.title, "Fix login bug");
        assert_eq!(output.change_type, "fix");
        assert_eq!(output.files_walkthrough.len(), 1);
    }

    #[test]
    fn test_parse_describe_fallback() {
        let output = parse_describe_response("Just a plain text response without YAML structure").unwrap();
        // Plain string is valid YAML but doesn't have the expected fields
        assert!(output.title.is_empty());
        assert!(output.files_walkthrough.is_empty());
    }
}
