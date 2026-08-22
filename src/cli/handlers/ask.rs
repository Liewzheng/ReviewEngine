use anyhow::Result;
use review_engine::models::*;

use super::output::write_output;
use super::review::{is_github_url, require_llm_configs};

pub async fn run_ask(
    question: &str,
    mr_url: &str,
    config_path: Option<String>,
    gitlab_token: Option<String>,
    github_token: Option<String>,
    llm_configs: Vec<String>,
    format: &str,
    output: &Option<String>,
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
    let result = review_engine::actions::ask::run_ask(&llm_client, &configs, question, &diff, &mr_info, None).await?;

    let md = format!(
        "## Ask\n\n**Question**: {}\n\n**Answer**: {}\n",
        question, result.answer
    );

    let review_out = ReviewOutput {
        reports: vec![ExpertReport {
            expert_name: "ask".to_string(),
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

/// Run `ask` against an in-memory diff string.
///
/// Shared by the `--diff` (file-backed) and `--stdin` (in-memory) paths so
/// neither ever passes the other's arguments through a file read.
/// AK-05 regression: `run_ask_stdin` passed the stdin diff as `question` and
/// the question string as `diff_path`, so every non-empty stdin + `--question`
/// failed with `No such file or directory (os error 2)`.
async fn run_ask_with_diff(
    question: &str,
    diff: &str,
    config_path: Option<String>,
    llm_configs: Vec<String>,
    format: &str,
    output: &Option<String>,
) -> Result<()> {
    let config_source = config_path.map(ConfigSource::Path);
    let config = review_engine::config::resolve_config(config_source).await?;
    let configs: Vec<LLMConfig> = require_llm_configs(&llm_configs, &config)?;

    let mr_info = MRInfo::new(
        "local".to_string(),
        "Local ask".to_string(),
        "local".to_string(),
        "main".to_string(),
    );

    let llm_client = review_engine::llm::client::LLMClient::new();
    let result = review_engine::actions::ask::run_ask(&llm_client, &configs, question, diff, &mr_info, None).await?;

    let md = format!(
        "## Ask\n\n**Question**: {}\n\n**Answer**: {}\n",
        question, result.answer
    );

    let review_out = ReviewOutput {
        reports: vec![ExpertReport {
            expert_name: "ask".to_string(),
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

pub async fn run_ask_local_diff(
    question: &str,
    diff_path: &str,
    config_path: Option<String>,
    llm_configs: Vec<String>,
    format: &str,
    output: &Option<String>,
) -> Result<()> {
    let diff = tokio::fs::read_to_string(diff_path).await?;
    run_ask_with_diff(question, &diff, config_path, llm_configs, format, output).await
}

pub async fn run_ask_local_repo(
    question: &str,
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
        println!("No changes to ask about (empty diff)");
        return Ok(());
    }

    let config_source = config_path.map(ConfigSource::Path);
    let config = review_engine::config::resolve_config(config_source).await?;
    let configs: Vec<LLMConfig> = require_llm_configs(&llm_configs, &config)?;

    let mr_info = MRInfo::new(
        local_path.to_string(),
        format!("Local ask: {}", local_path),
        "local".to_string(),
        base_ref.to_string(),
    );

    let llm_client = review_engine::llm::client::LLMClient::new();
    let result = review_engine::actions::ask::run_ask(&llm_client, &configs, question, &diff, &mr_info, None).await?;

    let md = format!(
        "## Ask\n\n**Question**: {}\n\n**Answer**: {}\n",
        question, result.answer
    );

    let review_out = ReviewOutput {
        reports: vec![ExpertReport {
            expert_name: "ask".to_string(),
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

pub async fn run_ask_stdin(
    question: &str,
    config_path: Option<String>,
    llm_configs: Vec<String>,
    format: &str,
    output: &Option<String>,
) -> Result<()> {
    use tokio::io::AsyncReadExt;
    let mut diff = String::new();
    tokio::io::stdin().read_to_string(&mut diff).await?;

    if diff.trim().is_empty() {
        println!("No diff provided on stdin");
        return Ok(());
    }

    run_ask_with_diff(question, &diff, config_path, llm_configs, format, output).await
}
