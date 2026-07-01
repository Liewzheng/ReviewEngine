//! Tool command that generates or updates CHANGELOG.md entries based on commit history and MR information.
//!
//!
//! @module review-engine
use crate::llm::client::LLMClient;
use crate::models::*;
use crate::prompt::PromptEngine;
use anyhow::Result;

/// A single changelog entry.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ChangelogEntry {
    pub change_type: String,
    pub description: String,
    pub scope: Option<String>,
}

/// Output from the update_changelog command.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ChangelogOutput {
    pub entries: Vec<ChangelogEntry>,
}

/// Run the update_changelog command: generate CHANGELOG entries.
pub async fn run_update_changelog(
    llm_client: &LLMClient,
    llm_configs: &[LLMConfig],
    diff: &str,
    commit_messages: &[String],
    mr_info: &MRInfo,
) -> Result<ChangelogOutput> {
    let prompt_engine = PromptEngine::new();
    let (system, user) = prompt_engine.build_changelog_prompt(diff, commit_messages, mr_info)?;
    let result = llm_client.complete_with_fallback(llm_configs, &system, &user).await?;
    parse_changelog_response(&result.content)
}

fn parse_changelog_response(response: &str) -> Result<ChangelogOutput> {
    let cleaned = crate::output::parser::clean_yaml(response);
    if let Ok(value) = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(&cleaned) {
        let entries = value["entries"]
            .as_sequence()
            .map(|seq| {
                seq.iter()
                    .map(|e| ChangelogEntry {
                        change_type: e["type"].as_str().unwrap_or("changed").to_string(),
                        description: e["description"].as_str().unwrap_or("").to_string(),
                        scope: e.get("scope").and_then(|v| v.as_str().map(String::from)),
                    })
                    .collect()
            })
            .unwrap_or_default();
        return Ok(ChangelogOutput { entries });
    }
    Ok(ChangelogOutput { entries: vec![] })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_changelog_yaml() {
        let yaml = r#"
entries:
  - type: "feat"
    description: "Add user authentication"
    scope: "auth"
  - type: "fix"
    description: "Fix token expiry"
    scope: null
"#;
        let output = parse_changelog_response(yaml).unwrap();
        assert_eq!(output.entries.len(), 2);
        assert_eq!(output.entries[0].change_type, "feat");
        assert_eq!(output.entries[0].scope.as_deref(), Some("auth"));
    }
}
