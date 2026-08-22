use anyhow::Result;
use review_engine::models::*;

use super::output::write_output;
use super::review::{is_github_url, require_llm_configs};

pub async fn run_improve(
    mr_url: &str,
    config_path: Option<String>,
    gitlab_token: Option<String>,
    github_token: Option<String>,
    llm_configs: Vec<String>,
    format: &str,
    output: &Option<String>,
    publish: bool,
) -> Result<()> {
    let token = if is_github_url(mr_url) {
        github_token.unwrap_or_else(|| std::env::var("GITHUB_TOKEN").unwrap_or_default())
    } else {
        gitlab_token.unwrap_or_else(|| std::env::var("GITLAB_TOKEN").unwrap_or_default())
    };
    let config_source = config_path.map(ConfigSource::Path);
    let config = review_engine::config::resolve_config(config_source).await?;
    let configs: Vec<LLMConfig> = require_llm_configs(&llm_configs, &config)?;

    let (diff, mr_info) = if is_github_url(mr_url) {
        let client = review_engine::git_provider::github::client::Client::new(&token, mr_url)?;
        let mr_info = client.fetch_pr_info().await?;
        let diff = client.fetch_diff().await?;
        (diff, mr_info)
    } else {
        let client = review_engine::git_provider::gitlab::client::Client::new(&token, mr_url)?;
        let mr_info = client.fetch_mr_info().await?;
        let diff = client.fetch_diff().await?;
        (diff, mr_info)
    };

    let llm_client = review_engine::llm::client::LLMClient::new();
    let result = review_engine::actions::improve::run_improve(&llm_client, &configs, &diff, &mr_info).await?;

    let md = format!(
        "## Code Improvement Suggestions\n\nGenerated {} suggestions.\n\n```json\n{}\n```",
        result.code_suggestions.len(),
        serde_json::to_string_pretty(&result).unwrap_or_default(),
    );

    let review_out = ReviewOutput {
        reports: vec![ExpertReport {
            expert_name: "improve".to_string(),
            findings: vec![],
            markdown: md,
            raw_llm_response: String::new(),
            parse_error: None,
            raw_dump_path: None,
        }],
        aggregated: None,
        dropped_findings: vec![],
        consolidated: None,
    };
    write_output(&review_out, format, output, None, None, false)?;

    if publish {
        if let Err(e) = review_engine::publish_review(&token, mr_url, &review_out).await {
            let msg = e.to_string();
            if msg.contains("401") || msg.contains("403") {
                eprintln!("error: --publish failed: token lacks write permissions.\n  {msg}");
            } else {
                eprintln!("error: --publish failed:\n  {msg}");
            }
            std::process::exit(1);
        }
    }

    Ok(())
}

pub async fn run_improve_local_diff(
    diff_path: &str,
    config_path: Option<String>,
    llm_configs: Vec<String>,
    format: &str,
    output: &Option<String>,
) -> Result<()> {
    let diff = tokio::fs::read_to_string(diff_path).await?;
    let config_source = config_path.map(ConfigSource::Path);
    let config = review_engine::config::resolve_config(config_source).await?;
    let configs: Vec<LLMConfig> = require_llm_configs(&llm_configs, &config)?;

    let mr_info = MRInfo::new(
        "local".to_string(),
        "Local improve".to_string(),
        "local".to_string(),
        "main".to_string(),
    );

    let llm_client = review_engine::llm::client::LLMClient::new();
    let result = review_engine::actions::improve::run_improve(&llm_client, &configs, &diff, &mr_info).await?;

    let md = format!(
        "## Code Improvement Suggestions\n\nGenerated {} suggestions.\n\n```json\n{}\n```",
        result.code_suggestions.len(),
        serde_json::to_string_pretty(&result).unwrap_or_default(),
    );

    let review_out = ReviewOutput {
        reports: vec![ExpertReport {
            expert_name: "improve".to_string(),
            findings: vec![],
            markdown: md,
            raw_llm_response: String::new(),
            parse_error: None,
            raw_dump_path: None,
        }],
        aggregated: None,
        dropped_findings: vec![],
        consolidated: None,
    };
    write_output(&review_out, format, output, None, None, false)?;

    Ok(())
}

pub async fn run_improve_local_repo(
    local_path: &str,
    base: Option<&str>,
    head: Option<&str>,
    staged: bool,
    since: Option<&str>,
    until: Option<&str>,
    config_path: Option<String>,
    llm_configs: Vec<String>,
    format: &str,
    output: &Option<String>,
) -> Result<()> {
    use review_engine::git::local::LocalGitBrowser;

    let base_ref = base.unwrap_or("main");
    let repo = LocalGitBrowser::new(local_path);
    let diff = repo.get_diff(base_ref, head, staged, since, until).await?;

    if diff.is_empty() {
        println!("No changes to improve (empty diff)");
        return Ok(());
    }

    let config_source = config_path.map(ConfigSource::Path);
    let config = review_engine::config::resolve_config(config_source).await?;
    let configs: Vec<LLMConfig> = require_llm_configs(&llm_configs, &config)?;

    let mr_info = MRInfo::new(
        local_path.to_string(),
        format!("Local improve: {}", local_path),
        "local".to_string(),
        base_ref.to_string(),
    );

    let llm_client = review_engine::llm::client::LLMClient::new();
    let result = review_engine::actions::improve::run_improve(&llm_client, &configs, &diff, &mr_info).await?;

    let md = format!(
        "## Code Improvement Suggestions\n\nGenerated {} suggestions.\n\n```json\n{}\n```",
        result.code_suggestions.len(),
        serde_json::to_string_pretty(&result).unwrap_or_default(),
    );

    let review_out = ReviewOutput {
        reports: vec![ExpertReport {
            expert_name: "improve".to_string(),
            findings: vec![],
            markdown: md,
            raw_llm_response: String::new(),
            parse_error: None,
            raw_dump_path: None,
        }],
        aggregated: None,
        dropped_findings: vec![],
        consolidated: None,
    };
    write_output(&review_out, format, output, None, None, false)?;

    Ok(())
}
