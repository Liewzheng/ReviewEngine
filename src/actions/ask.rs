use crate::llm::client::LLMClient;
use crate::models::*;
use crate::prompt::PromptEngine;
use anyhow::Result;

/// Output from the ask command.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AskOutput {
    pub answer: String,
}

/// Run the ask command: answer a question about the codebase.
pub async fn run_ask(
    llm_client: &LLMClient,
    llm_configs: &[LLMConfig],
    question: &str,
    diff: &str,
    mr_info: &MRInfo,
    _file_content: Option<&str>,
) -> Result<AskOutput> {
    let prompt_engine = PromptEngine::new();
    let (system, user) = prompt_engine.build_ask_prompt(question, diff, mr_info)?;
    let result = llm_client.complete_with_fallback(llm_configs, &system, &user).await?;
    Ok(AskOutput { answer: result.content })
}

/// Run the ask_line command: answer about a specific line in a file.
pub async fn run_ask_line(
    llm_client: &LLMClient,
    llm_configs: &[LLMConfig],
    question: &str,
    file: &str,
    line: u32,
    file_content: &str,
) -> Result<AskOutput> {
    let prompt_engine = PromptEngine::new();
    let (system, user) = prompt_engine.build_ask_line_prompt(question, file, line, file_content)?;
    let result = llm_client.complete_with_fallback(llm_configs, &system, &user).await?;
    Ok(AskOutput { answer: result.content })
}
