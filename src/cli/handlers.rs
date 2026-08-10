use anyhow::Result;
use review_engine::models::*;
use review_engine::progress::ProgressMap;
use review_engine::upgrade::check_for_updates_with_version;
use review_engine::upgrade::download::{download_asset, download_verified_asset};
use review_engine::upgrade::verify::{extract_asset, parse_sha256_line, verify_file_sha256};
use review_engine::upgrade::{current_asset_spec, InstallMethod, Release, ReleaseAsset, UpdateCheck, Version};
use std::path::{Path, PathBuf};

/// Resolve LLM configuration from multiple sources:
/// 1. CLI --llm-config arguments (highest priority)
/// 2. LLM_CONFIG environment variable
/// 3. config.llm from parsed config file
/// 4. Empty vec (fallback)
pub fn resolve_llm_configs(argv_llm_configs: &[String], config: &AppConfig) -> anyhow::Result<Vec<LLMConfig>> {
    if !argv_llm_configs.is_empty() {
        let mut configs = Vec::new();
        for s in argv_llm_configs {
            configs.push(serde_json::from_str::<LLMConfig>(s)?);
        }
        return Ok(configs);
    }
    let env_configs = review_engine::config::llm_configs_from_env();
    if !env_configs.is_empty() {
        return Ok(env_configs);
    }
    if !config.llm.is_empty() {
        return Ok(config.llm.clone());
    }
    Ok(Vec::new())
}

fn is_github_url(url: &str) -> bool {
    url.contains(".github.") || url.contains("github.com")
}

pub async fn run_stdin(format: &str, output: &Option<String>) -> Result<()> {
    use tokio::io::AsyncReadExt;
    let mut buf = String::new();
    tokio::io::stdin().read_to_string(&mut buf).await?;
    let req: serde_json::Value = serde_json::from_str(&buf)?;

    let mr_url = req["mr_url"].as_str().unwrap_or_default();
    let token = req["github_token"]
        .as_str()
        .or_else(|| req["gitlab_token"].as_str())
        .unwrap_or_default();
    let llm_configs: Vec<LLMConfig> = serde_json::from_value(req["llm_configs"].clone())?;
    let config_toml = req["config"].as_str().map(|s| s.to_string());

    let result =
        review_engine::run_review(mr_url, token, llm_configs, config_toml.map(ConfigSource::Inline), None).await?;
    // The verification-enabled flag is resolved inside `run_review` and not
    // available here; `false` keeps the historical list-only appendix.
    write_output(&result, format, output, None, None, false)?;

    Ok(())
}

pub async fn run_mr(
    mr_url: &str,
    config_path: Option<String>,
    gitlab_token: Option<String>,
    github_token: Option<String>,
    llm_configs: Vec<String>,
    format: &str,
    output: &Option<String>,
    publish: bool,
    progress_map: Option<ProgressMap>,
    review_id: &str,
) -> Result<()> {
    let token = if is_github_url(mr_url) {
        github_token.unwrap_or_else(|| std::env::var("GITHUB_TOKEN").unwrap_or_default())
    } else {
        gitlab_token.unwrap_or_else(|| std::env::var("GITLAB_TOKEN").unwrap_or_default())
    };
    let config_source = config_path.map(ConfigSource::Path);
    let config = review_engine::config::resolve_config(config_source.clone()).await?;
    let configs: Vec<LLMConfig> = resolve_llm_configs(&llm_configs, &config)?;

    let progress_override = progress_map.map(|map| (map, review_id.to_string()));
    let result = review_engine::run_review(mr_url, &token, configs, config_source, progress_override).await?;
    write_output(
        &result,
        format,
        output,
        None,
        Some(&config.output_dir),
        config.report.verification_pass,
    )?;

    if publish {
        if let Err(e) = review_engine::publish_review(&token, mr_url, &result).await {
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
    let configs: Vec<LLMConfig> = resolve_llm_configs(&llm_configs, &config)?;

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
    let configs: Vec<LLMConfig> = resolve_llm_configs(&llm_configs, &config)?;

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
    let configs: Vec<LLMConfig> = resolve_llm_configs(&llm_configs, &config)?;

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
        }],
        aggregated: None,
        dropped_findings: vec![],
        consolidated: None,
    };
    write_output(&review_out, format, output, None, None, false)?;

    Ok(())
}

pub async fn run_describe(
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
    let configs: Vec<LLMConfig> = resolve_llm_configs(&llm_configs, &config)?;

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
    let commit_messages = vec![];
    let result =
        review_engine::actions::describe::run_describe(&llm_client, &configs, &diff, &mr_info, &commit_messages)
            .await?;

    let md = format!(
        "## PR Description\n\n**Title**: {}\n\n**Description**: {}\n\n**Type**: {}",
        result.title, result.description, result.change_type,
    );

    let review_out = ReviewOutput {
        reports: vec![ExpertReport {
            expert_name: "describe".to_string(),
            findings: vec![],
            markdown: md,
            raw_llm_response: String::new(),
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

pub async fn run_describe_local_diff(
    diff_path: &str,
    config_path: Option<String>,
    llm_configs: Vec<String>,
    format: &str,
    output: &Option<String>,
) -> Result<()> {
    let diff = tokio::fs::read_to_string(diff_path).await?;
    let config_source = config_path.map(ConfigSource::Path);
    let config = review_engine::config::resolve_config(config_source).await?;
    let configs: Vec<LLMConfig> = resolve_llm_configs(&llm_configs, &config)?;

    let mr_info = MRInfo::new(
        "local".to_string(),
        "Local describe".to_string(),
        "local".to_string(),
        "main".to_string(),
    );

    let llm_client = review_engine::llm::client::LLMClient::new();
    let commit_messages = vec![];
    let result =
        review_engine::actions::describe::run_describe(&llm_client, &configs, &diff, &mr_info, &commit_messages)
            .await?;

    let md = format!(
        "## PR Description\n\n**Title**: {}\n\n**Description**: {}\n\n**Type**: {}",
        result.title, result.description, result.change_type,
    );

    let review_out = ReviewOutput {
        reports: vec![ExpertReport {
            expert_name: "describe".to_string(),
            findings: vec![],
            markdown: md,
            raw_llm_response: String::new(),
        }],
        aggregated: None,
        dropped_findings: vec![],
        consolidated: None,
    };
    write_output(&review_out, format, output, None, None, false)?;

    Ok(())
}

pub async fn run_describe_local_repo(
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
        println!("No changes to describe (empty diff)");
        return Ok(());
    }

    let config_source = config_path.map(ConfigSource::Path);
    let config = review_engine::config::resolve_config(config_source).await?;
    let configs: Vec<LLMConfig> = resolve_llm_configs(&llm_configs, &config)?;

    let mr_info = MRInfo::new(
        local_path.to_string(),
        format!("Local describe: {}", local_path),
        "local".to_string(),
        base_ref.to_string(),
    );

    let llm_client = review_engine::llm::client::LLMClient::new();
    let commit_messages = vec![];
    let result =
        review_engine::actions::describe::run_describe(&llm_client, &configs, &diff, &mr_info, &commit_messages)
            .await?;

    let md = format!(
        "## PR Description\n\n**Title**: {}\n\n**Description**: {}\n\n**Type**: {}",
        result.title, result.description, result.change_type,
    );

    let review_out = ReviewOutput {
        reports: vec![ExpertReport {
            expert_name: "describe".to_string(),
            findings: vec![],
            markdown: md,
            raw_llm_response: String::new(),
        }],
        aggregated: None,
        dropped_findings: vec![],
        consolidated: None,
    };
    write_output(&review_out, format, output, None, None, false)?;

    Ok(())
}

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
    let configs: Vec<LLMConfig> = resolve_llm_configs(&llm_configs, &config)?;

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
    let configs: Vec<LLMConfig> = resolve_llm_configs(&llm_configs, &config)?;

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
    let configs: Vec<LLMConfig> = resolve_llm_configs(&llm_configs, &config)?;

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
        }],
        aggregated: None,
        dropped_findings: vec![],
        consolidated: None,
    };
    write_output(&review_out, format, output, None, None, false)?;

    Ok(())
}

pub async fn run_update_changelog(
    local_path: &str,
    since: Option<&str>,
    until: Option<&str>,
    config_path: Option<String>,
    llm_configs: Vec<String>,
    format: &str,
    output: &Option<String>,
) -> Result<()> {
    use review_engine::git::local::LocalGitBrowser;

    let repo = LocalGitBrowser::new(local_path);
    let diff = repo.get_diff("main", None, false, since, until).await?;

    if diff.is_empty() {
        println!("No changes to changelog (empty diff)");
        return Ok(());
    }

    let config_source = config_path.map(ConfigSource::Path);
    let config = review_engine::config::resolve_config(config_source).await?;
    let configs: Vec<LLMConfig> = resolve_llm_configs(&llm_configs, &config)?;

    let mr_info = MRInfo::new(
        local_path.to_string(),
        format!("Local changelog: {}", local_path),
        "local".to_string(),
        "main".to_string(),
    );

    let llm_client = review_engine::llm::client::LLMClient::new();
    let commit_messages = vec![];
    let result = review_engine::actions::update_changelog::run_update_changelog(
        &llm_client,
        &configs,
        &diff,
        &commit_messages,
        &mr_info,
    )
    .await?;

    let md = format!(
        "## Changelog Update\n\n{} entries generated.\n\n```json\n{}\n```",
        result.entries.len(),
        serde_json::to_string_pretty(&result).unwrap_or_default(),
    );

    let review_out = ReviewOutput {
        reports: vec![ExpertReport {
            expert_name: "update_changelog".to_string(),
            findings: vec![],
            markdown: md,
            raw_llm_response: String::new(),
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

pub async fn run_local(
    diff_path: &str,
    local_path: Option<&str>,
    config_path: Option<String>,
    llm_configs: Vec<String>,
    format: &str,
    output: &Option<String>,
    progress_map: Option<ProgressMap>,
    review_id: &str,
) -> Result<()> {
    let diff = tokio::fs::read_to_string(diff_path).await?;
    let config_source = config_path.map(ConfigSource::Path);
    let config = review_engine::config::resolve_config(config_source).await?;
    let llm_configs: Vec<LLMConfig> = resolve_llm_configs(&llm_configs, &config)?;

    // Root cause C: propagate the real `--local-path` (defaulting to the
    // current directory) instead of the placeholder "local", so the "Full
    // File Contents" injection and the verification pass read from the actual
    // checkout — restoring full-file context for chunked large-PR reviews.
    // When the path is wrong/unreadable the injection fails open (empty
    // section), as for remote reviews.
    let project_path = local_path.unwrap_or(".");
    let (experts, mr_info) = prepare_review(&config, project_path, "local", "main");

    let (reports, _, dropped_findings, consolidated) = review_engine::team::orchestrator::run_experts(
        &experts,
        &mr_info,
        &diff,
        &llm_configs,
        &config,
        progress_map.clone(),
        review_id,
    )
    .await?;

    let out = ReviewOutput::new(reports)
        .with_dropped_findings(dropped_findings)
        .with_consolidated(consolidated);
    write_output(
        &out,
        format,
        output,
        None,
        Some(&config.output_dir),
        config.report.verification_pass,
    )?;
    review_engine::progress::complete_progress(progress_map.as_ref(), review_id);
    Ok(())
}

fn prepare_review(
    config: &AppConfig,
    project_path: &str,
    source_branch: &str,
    target_branch: &str,
) -> (Vec<ExpertDef>, MRInfo) {
    let experts = config.build_expert_defs();
    let mr_info = MRInfo::new(
        project_path.to_string(),
        format!("Local review: {}", project_path),
        source_branch.to_string(),
        target_branch.to_string(),
    );
    (experts, mr_info)
}

pub async fn run_local_repo(
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
    progress_map: Option<ProgressMap>,
    review_id: &str,
) -> Result<()> {
    use review_engine::git::local::LocalGitBrowser;

    let base_ref = base.unwrap_or("main");
    let repo = LocalGitBrowser::new(local_path);
    let diff = repo.get_diff(base_ref, head, staged, since, until).await?;

    if diff.is_empty() {
        println!("No changes to review (empty diff)");
        return Ok(());
    }

    let config_source = config_path.map(ConfigSource::Path);
    let config = review_engine::config::resolve_config(config_source).await?;

    let llm_configs: Vec<LLMConfig> = resolve_llm_configs(&llm_configs, &config)?;

    if llm_configs.is_empty() {
        anyhow::bail!(
            "No LLM configuration found. \
             Provide [[llm]] in ~/.config/review-engine/.code-audit-config.toml, \
             the project .code-audit-config.toml, --llm-config, or LLM_CONFIG env var."
        );
    }

    let (experts, mr_info) = prepare_review(&config, local_path, "local", base_ref);

    let (reports, _, dropped_findings, consolidated) = review_engine::team::orchestrator::run_experts(
        &experts,
        &mr_info,
        &diff,
        &llm_configs,
        &config,
        progress_map.clone(),
        review_id,
    )
    .await?;

    let out = ReviewOutput::new(reports)
        .with_dropped_findings(dropped_findings)
        .with_consolidated(consolidated);

    let repo_root = match std::fs::canonicalize(local_path) {
        Ok(p) => Some(p),
        Err(e) => {
            tracing::warn!(
                "Failed to canonicalize local path '{}': {}; path normalization disabled",
                local_path,
                e
            );
            None
        }
    };
    write_output(
        &out,
        format,
        output,
        repo_root.as_deref(),
        Some(&config.output_dir),
        config.report.verification_pass,
    )?;
    review_engine::progress::complete_progress(progress_map.as_ref(), review_id);
    Ok(())
}

/// Run a full-content review of every controlled file under `path` inside
/// `local_path` (P0: `review --path <dir> --local-path <repo>`).
///
/// Unlike `--diff`/`--base` (which review changes) and `audit` (which runs
/// the whole-repository static+LLM pipeline), this entry point builds a
/// synthetic "empty tree → current" diff for the subdirectory and reviews
/// every line of every file through the standard expert team, so the
/// large-PR coverage guarantee applies. A zero-finding result appends an
/// explicit credibility note (P1) instead of reading as "the code is clean".
pub async fn run_local_path(
    path: &str,
    local_path: &str,
    config_path: Option<String>,
    llm_configs: Vec<String>,
    format: &str,
    output: &Option<String>,
    progress_map: Option<ProgressMap>,
    review_id: &str,
) -> Result<()> {
    let config_source = config_path.map(ConfigSource::Path);
    let config = review_engine::config::resolve_config(config_source).await?;
    let llm_configs: Vec<LLMConfig> = resolve_llm_configs(&llm_configs, &config)?;

    // Validate the review input FIRST: a bad --path (missing directory, empty
    // tree, traversal) must fail with the path error regardless of LLM
    // configuration. The LLM check below otherwise masks it with an unrelated
    // "No LLM configuration found" message in environments that resolve no
    // provider (e.g. clean CI) — and the actionable error is the path one.
    let full = review_engine::input::full_path::build_path_review_diff(local_path, path)?;
    let file_count = full.files.len();

    if llm_configs.is_empty() {
        anyhow::bail!(
            "No LLM configuration found. \
             Provide [[llm]] in ~/.config/review-engine/.code-audit-config.toml, \
             the project .code-audit-config.toml, --llm-config, or LLM_CONFIG env var."
        );
    }

    let (experts, mr_info) = prepare_review(&config, local_path, "local", "main");

    let (reports, _, dropped_findings, consolidated) = review_engine::team::orchestrator::run_experts(
        &experts,
        &mr_info,
        &full.diff,
        &llm_configs,
        &config,
        progress_map.clone(),
        review_id,
    )
    .await?;

    let mut out = ReviewOutput::new(reports)
        .with_dropped_findings(dropped_findings)
        .with_consolidated(consolidated);

    // P1: a full-content review that finds nothing must not read as "the
    // code is clean". Surface the coverage claim explicitly.
    if out.reports.iter().map(|r| r.findings.len()).sum::<usize>() == 0 {
        out.reports.push(ExpertReport {
            expert_name: "path_review".to_string(),
            findings: vec![],
            markdown: format!(
                "## Full-Content Path Review\n\n\
                 This review covered **{} file(s)** under `{}` in full (synthetic empty-tree diff).\n\n\
                 **Zero findings does not mean the code is problem-free** — 本次为 {} 个文件的全量内容审查，零发现不代表代码无问题。\
                 Coverage is bounded by the model context window and the configured token budget.\n",
                file_count, path, file_count
            ),
            raw_llm_response: String::new(),
        });
    }

    write_output(
        &out,
        format,
        output,
        None,
        Some(&config.output_dir),
        config.report.verification_pass,
    )?;
    review_engine::progress::complete_progress(progress_map.as_ref(), review_id);
    Ok(())
}

pub async fn run_repo_review_local_or_enhanced(
    local_path: &str,
    llm_configs: &[LLMConfig],
    format: &str,
    output: &Option<String>,
    progress_map: Option<ProgressMap>,
    review_id: &str,
    config: &AppConfig,
) -> Result<()> {
    use review_engine::repo::RepoScanner;

    // The verification pass only runs on the LLM-enhanced path.
    let verification_enabled = !llm_configs.is_empty() && config.report.verification_pass;
    let config = Some(std::sync::Arc::new(config.clone()));
    let result = if llm_configs.is_empty() {
        // Local-only analysis (no LLM)
        review_engine::actions::repo_review::run_local_repo_review(local_path, progress_map, review_id, config).await?
    } else {
        // LLM-enhanced analysis
        let scanner = RepoScanner::new(local_path);
        let entries = scanner.scan()?;
        let llm_client = review_engine::llm::client::LLMClient::new();
        review_engine::actions::repo_review::run_repo_review(
            &llm_client,
            llm_configs,
            local_path,
            &entries,
            progress_map,
            review_id,
            config,
        )
        .await?
    };

    let text = review_engine::actions::repo_review::render_repo_review_output(&result, format, verification_enabled)?;
    match output {
        Some(path) => std::fs::write(path, &text)?,
        None => println!("{}", text),
    }
    Ok(())
}

/// Normalize all finding file paths in a ReviewOutput in-place,
/// then re-render markdown with the normalized paths.
fn normalize_all_findings(output: &mut ReviewOutput, repo_root: &Path) {
    for report in &mut output.reports {
        for finding in &mut report.findings {
            finding.file = review_engine::output::path::normalize_path(&finding.file, Some(repo_root));
        }
        report.markdown =
            review_engine::output::renderer::render_expert_markdown(&report.expert_name, &report.findings);
    }
    if let Some(ref mut agg) = output.aggregated {
        for finding in &mut agg.findings {
            finding.file = review_engine::output::path::normalize_path(&finding.file, Some(repo_root));
        }
        agg.markdown = review_engine::output::renderer::render_aggregated_markdown(&agg.findings);
    }
}

/// Format a ReviewOutput according to the requested format string.
///
/// `verification_enabled` tells the Markdown renderer whether the finding
/// verification pass ran, so the "Dropped by verification" appendix can show
/// a run summary even when nothing was dropped. When `result.consolidated`
/// is present, a "Lead Summary" section is rendered after the per-expert
/// reports and before that appendix.
fn format_output(result: &ReviewOutput, format: &str, verification_enabled: bool) -> Result<String> {
    Ok(match format {
        "markdown" => {
            let text = result
                .reports
                .iter()
                .map(|r| r.markdown.clone())
                .collect::<Vec<_>>()
                .join("\n\n---\n\n");
            let mut text = if text.trim().is_empty() {
                "# PR Review Report\n\nNo review content was generated. \
                 Check that LLM configuration is correct and that the diff contains changes.\n"
                    .to_string()
            } else {
                text
            };
            let checked =
                result.reports.iter().map(|r| r.findings.len()).sum::<usize>() + result.dropped_findings.len();
            // Lead consolidation summary: after the per-expert reports,
            // before the "Dropped by verification" appendix.
            if let Some(ref consolidated) = result.consolidated {
                if !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str("\n---\n\n");
                text.push_str(&review_engine::output::team_renderer::render_lead_summary(consolidated));
            }
            let appendix = review_engine::output::renderer::render_dropped_findings_appendix(
                &result.dropped_findings,
                verification_enabled,
                checked,
            );
            if !appendix.is_empty() {
                if !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str("\n---\n\n");
                text.push_str(&appendix);
            }
            text
        }
        "aggregated-markdown" => {
            let text = result
                .aggregated
                .as_ref()
                .map(|a| a.markdown.clone())
                .unwrap_or_else(|| String::from("No aggregated report"));
            if text.trim().is_empty() {
                "# Aggregated PR Review Report\n\nNo aggregated review content was generated. \
                 Check that LLM configuration is correct and that the diff contains changes.\n"
                    .to_string()
            } else {
                text
            }
        }
        _ => serde_json::to_string_pretty(result)?,
    })
}

fn write_output(
    result: &ReviewOutput,
    format: &str,
    output: &Option<String>,
    repo_root: Option<&Path>,
    output_dir: Option<&str>,
    verification_enabled: bool,
) -> Result<()> {
    let text = if let Some(root) = repo_root {
        let mut normalized = result.clone();
        normalize_all_findings(&mut normalized, root);
        format_output(&normalized, format, verification_enabled)?
    } else {
        format_output(result, format, verification_enabled)?
    };

    match output {
        Some(path) => {
            // Explicit --output: validate path to prevent directory traversal
            let path = std::path::Path::new(path);
            for component in path.components() {
                if let std::path::Component::ParentDir = component {
                    anyhow::bail!("--output path must not contain '..'");
                }
            }
            std::fs::create_dir_all(path.parent().unwrap_or(path))?;
            std::fs::write(path, &text)?;
        }
        None => {
            // No explicit output: print to stdout
            println!("{}", text);
            // And save to default directory if configured
            if let Some(dir) = output_dir {
                let dir = std::path::Path::new(dir);
                // Validate output_dir to prevent directory traversal
                for component in dir.components() {
                    if let std::path::Component::ParentDir = component {
                        anyhow::bail!("output_dir must not contain '..'");
                    }
                }
                if !dir.exists() {
                    std::fs::create_dir_all(dir)?;
                }
                let ext = match format {
                    "markdown" | "aggregated-markdown" => "md",
                    _ => "json",
                };
                let now = chrono::Local::now().format("%Y%m%d_%H%M%S");
                let filename = format!("review_{}.{}", now, ext);
                let filepath = dir.join(&filename);
                std::fs::write(&filepath, &text)?;
                eprintln!("Report saved to {}", filepath.display());
            }
        } // None
    } // match output

    Ok(())
}

/// Watch a config file for changes and log a warning when modified.
/// This allows users to restart the app to pick up changes.
pub async fn watch_config_file(path: std::path::PathBuf) {
    tokio::task::spawn_blocking(move || {
        use notify::{EventKind, Watcher};
        use std::sync::mpsc;

        let (tx, rx) = mpsc::channel::<notify::Result<notify::Event>>();
        let mut watcher = match notify::recommended_watcher(tx) {
            Ok(w) => w,
            Err(e) => {
                tracing::warn!("Failed to start config watcher: {}", e);
                return;
            }
        };

        if let Err(e) = watcher.watch(&path, notify::RecursiveMode::NonRecursive) {
            tracing::warn!("Failed to watch config file: {}", e);
            return;
        }

        loop {
            match rx.recv() {
                Ok(Ok(event)) => {
                    if matches!(event.kind, EventKind::Modify(_) | EventKind::Create(_)) {
                        tracing::warn!(
                            "Config file '{}' has changed. Restart review-engine to apply changes.",
                            path.display()
                        );
                    }
                }
                Ok(Err(e)) => {
                    tracing::debug!("Config watcher error: {}", e);
                }
                Err(_) => break,
            }
        }
    })
    .await
    .ok();
}

// ───────────────────────────────────────────────────────────────────────
// `reng upgrade` — self-update (check / install-method hint / self-replace).
//
// The upgrade library (`review_engine::upgrade`) owns release lookup,
// download verification and extraction. This layer owns the CLI UX, the
// install-method dispatch, the concurrent-upgrade lock, and the atomic
// self-replace + rollback of the running binary.
//
// Test seams (documented env overrides, inert in normal use):
//   REVIEW_UPGRADE_TEST_RELEASE     inject release metadata as JSON instead
//                                   of querying the GitHub API
//   REVIEW_UPGRADE_CURRENT_VERSION  fake the "current" version (default: pkg)
//   REVIEW_UPGRADE_INSTALL_METHOD   force brew/cargo/docker/plain/unknown
//   REVIEW_UPGRADE_EXE              override the target exe path (self-replace
//                                   against a temp fixture instead of $0)
// ───────────────────────────────────────────────────────────────────────

const ENV_TEST_RELEASE: &str = "REVIEW_UPGRADE_TEST_RELEASE";
const ENV_CURRENT_VERSION: &str = "REVIEW_UPGRADE_CURRENT_VERSION";
const ENV_INSTALL_METHOD: &str = "REVIEW_UPGRADE_INSTALL_METHOD";
const ENV_EXE_OVERRIDE: &str = "REVIEW_UPGRADE_EXE";

const LOCK_FILE_NAME: &str = ".review-engine.upgrade.lock";
/// A lock older than this (seconds) is considered stale and can be reclaimed.
const LOCK_STALE_SECS: u64 = 600;

#[derive(serde::Deserialize)]
struct TestReleaseOverride {
    tag: String,
    asset_name: String,
    asset_url: String,
    asset_size: u64,
    checksum_url: String,
    checksum_size: u64,
}

/// `reng upgrade` entry point.
///
/// * `--check` / default first screen: report what's available.
/// * Plain installs perform an in-place self-replace (confirmed unless `--yes`).
/// * Brew: hint only, or execute `brew upgrade` when `--yes`.
/// * Cargo / Docker / Unknown: hint only, never auto-execute.
/// * `--version <tag>`: target a specific release; only the latest release is
///   auto-installable by the built-in updater.
/// * `--rollback`: restore `review-engine.bak` over the current binary.
pub async fn run_upgrade(check_only: bool, yes: bool, target_version: Option<&str>, rollback: bool) -> Result<()> {
    if rollback {
        return run_rollback();
    }

    let check = resolve_update_check().await?;

    // Explicit target version (--version <tag>).
    if let Some(tag) = target_version {
        let target = Version::parse_release_tag(tag).ok_or_else(|| {
            anyhow::anyhow!("invalid target version {tag:?}: expected a stable vMAJOR.MINOR.PATCH tag")
        })?;
        if target <= check.current_version {
            println!("review-engine is up to date (v{target})");
            return Ok(());
        }
        if target != check.latest_version {
            anyhow::bail!(
                "cannot auto-upgrade to v{target}: only the latest release v{} is supported by the built-in updater; run without --version",
                check.latest_version
            );
        }
    }

    // First screen (check mode or default): always report what's available.
    if check.has_update {
        println!(
            "A newer version of review-engine is available ({} -> {}).",
            check.current_version, check.latest_version
        );
        println!(
            "Detected install source: {}.",
            install_source_label(check.install_method)
        );
        println!("To update, run: {}", check.upgrade_command());
    } else {
        println!("review-engine is up to date (v{})", check.current_version);
        return Ok(());
    }

    if check_only {
        return Ok(());
    }

    // Dispatch by install method.
    match check.install_method {
        InstallMethod::Plain => {
            if !yes && !confirm_upgrade(&check.latest_version.to_string())? {
                println!("upgrade aborted.");
                return Ok(());
            }
            run_plain_upgrade(&check).await?;
        }
        InstallMethod::Brew if yes => run_brew_upgrade()?,
        InstallMethod::Brew => {
            println!("Run again with --yes to execute `brew upgrade review-engine`.");
        }
        InstallMethod::Cargo | InstallMethod::Docker | InstallMethod::Unknown => {}
    }
    Ok(())
}

fn install_source_label(method: InstallMethod) -> &'static str {
    match method {
        InstallMethod::Brew => "Homebrew",
        InstallMethod::Cargo => "Cargo (~/.cargo/bin)",
        InstallMethod::Docker => "Docker 容器",
        InstallMethod::Plain => "直接部署的二进制",
        InstallMethod::Unknown => "未知（手动安装）",
    }
}

fn current_version() -> String {
    std::env::var(ENV_CURRENT_VERSION).unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_string())
}

fn resolve_install_method() -> InstallMethod {
    let Ok(v) = std::env::var(ENV_INSTALL_METHOD) else {
        return InstallMethod::detect();
    };
    match v.to_ascii_lowercase().as_str() {
        "brew" => InstallMethod::Brew,
        "cargo" => InstallMethod::Cargo,
        "docker" => InstallMethod::Docker,
        "plain" => InstallMethod::Plain,
        _ => InstallMethod::Unknown,
    }
}

/// Resolve the executable to replace. Always canonicalized first: on macOS
/// `std::env::current_exe()` returns the *symlink invocation path* (e.g.
/// `.../bin/reng`), not the real binary — upgrading the link would replace the
/// symlink with a real file and leave the actual `review-engine` untouched.
/// `REVIEW_UPGRADE_EXE` is the test seam that also feeds this path, so it is
/// canonicalized the same way. Falls back to the raw path if it cannot be
/// resolved.
fn current_exe_path() -> PathBuf {
    let raw = match std::env::var_os(ENV_EXE_OVERRIDE) {
        Some(p) => PathBuf::from(p),
        None => std::env::current_exe().unwrap_or_else(|_| PathBuf::from("review-engine")),
    };
    canonical_exe_path(&raw)
}

/// Canonicalize `raw` so a symlink invocation path resolves to the real
/// binary; falls back to the raw path when it cannot be resolved.
fn canonical_exe_path(raw: &Path) -> PathBuf {
    std::fs::canonicalize(raw).unwrap_or_else(|_| raw.to_path_buf())
}

fn test_release_override() -> Option<TestReleaseOverride> {
    let raw = std::env::var(ENV_TEST_RELEASE).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Resolve the update check: from the test override when set, otherwise from
/// the real GitHub API. The detected install method is (re)applied so the
/// `REVIEW_UPGRADE_INSTALL_METHOD` override wins over `InstallMethod::detect()`.
async fn resolve_update_check() -> Result<UpdateCheck> {
    let current = current_version();
    if let Some(t) = test_release_override() {
        let current_version = Version::parse(&current)?;
        let latest_version = Version::parse_release_tag(&t.tag)
            .ok_or_else(|| anyhow::anyhow!("invalid test release tag {:?}", t.tag))?;
        let asset = ReleaseAsset {
            name: t.asset_name.clone(),
            download_url: t.asset_url,
            size: t.asset_size,
        };
        let checksum = ReleaseAsset {
            name: format!("{}.sha256", t.asset_name),
            download_url: t.checksum_url,
            size: t.checksum_size,
        };
        let release = Release {
            tag_name: t.tag.clone(),
            html_url: format!("https://github.com/Liewzheng/ReviewEngine/releases/tag/{}", t.tag),
            published_at: String::new(),
            assets: vec![asset.clone(), checksum.clone()],
        };
        return Ok(UpdateCheck {
            current_version,
            latest_version,
            has_update: latest_version > current_version,
            platform: current_asset_spec().ok(),
            asset: Some(asset),
            checksum_asset: Some(checksum),
            install_method: resolve_install_method(),
            latest_release: release,
        });
    }
    let mut check = check_for_updates_with_version(&current).await?;
    check.install_method = resolve_install_method();
    Ok(check)
}

fn confirm_upgrade(target: &str) -> Result<bool> {
    use std::io::Write;
    print!("Proceed with the upgrade to v{target}? [y/N] ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let answer = line.trim().to_ascii_lowercase();
    Ok(answer == "y" || answer == "yes")
}

/// Execute `brew upgrade review-engine`, passing brew's output straight
/// through to the terminal. Fails with brew's exit status on error.
fn run_brew_upgrade() -> Result<()> {
    println!("Running: brew upgrade review-engine");
    let status = std::process::Command::new("brew")
        .args(["upgrade", "review-engine"])
        .status()?;
    if !status.success() {
        anyhow::bail!(
            "`brew upgrade review-engine` failed (exit status {:?}); the output above is from brew",
            status.code()
        );
    }
    Ok(())
}

/// Restore the previous binary from `review-engine.bak`.
fn run_rollback() -> Result<()> {
    let exe = current_exe_path();
    let dir = exe
        .parent()
        .ok_or_else(|| anyhow::anyhow!("cannot determine the directory of {}", exe.display()))?;
    let bak = dir.join("review-engine.bak");
    if !bak.exists() {
        anyhow::bail!("no backup found at {}; nothing to roll back", bak.display());
    }
    if exe.exists() {
        std::fs::remove_file(&exe)?;
    }
    std::fs::rename(&bak, &exe)?;
    set_executable(&exe)?;
    println!("rolled back to the previous binary at {}", exe.display());
    Ok(())
}

/// In-place self-replace: download → extract → backup → install → verify →
/// smoke → keep `.bak`. Every failure after the backup restores the previous
/// binary and preserves the `.bak` for a later `--rollback`.
async fn run_plain_upgrade(check: &UpdateCheck) -> Result<()> {
    let exe = current_exe_path();
    let exe_dir = exe
        .parent()
        .ok_or_else(|| anyhow::anyhow!("cannot determine the directory of {}", exe.display()))?
        .to_path_buf();
    let asset = check
        .asset
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("no release asset for this platform; cannot auto-upgrade"))?;
    let checksum = check.checksum_asset.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "release has no checksum sidecar for {}; cannot auto-upgrade",
            asset.name
        )
    })?;
    let platform = check
        .platform
        .ok_or_else(|| anyhow::anyhow!("unsupported platform; cannot auto-upgrade"))?;
    if !exe.is_file() {
        anyhow::bail!("current executable not found at {}; cannot upgrade", exe.display());
    }

    // Serialize concurrent upgrades; the lock file is removed on drop.
    let _lock = UpgradeLock::acquire(&exe_dir)?;

    println!("downloading {}", asset.name);
    let archive = download_verified_asset(asset, checksum, &exe_dir).await?;
    println!("verifying checksum of {}", asset.name);

    // Extract into a temp dir next to the exe (same filesystem → atomic rename).
    let extract_dir = unique_temp_dir(&exe_dir, "extract")?;
    let _cleanup = CleanupPaths(vec![archive.clone(), extract_dir.clone()]);
    extract_asset(&archive, platform.format, &extract_dir)
        .map_err(|e| anyhow::anyhow!("failed to extract release archive: {e}"))?;

    let exe_name = if platform.is_windows() {
        "review-engine.exe"
    } else {
        "review-engine"
    };
    let extracted = find_binary_in(&extract_dir, exe_name)
        .ok_or_else(|| anyhow::anyhow!("no {exe_name} found inside the release archive"))?;

    // Back up the current binary before touching it.
    println!("installing");
    let bak = exe_dir.join("review-engine.bak");
    if bak.exists() {
        let _ = std::fs::remove_file(&bak);
    }
    std::fs::rename(&exe, &bak)?;

    if let Err(e) = install_binary(&extracted, &exe) {
        rollback_restore(&exe, &bak);
        return Err(anyhow::anyhow!(
            "failed to install the new binary: {e}; previous version restored (backup kept at {})",
            bak.display()
        ));
    }

    // 双保险: re-verify the installed binary against a downloaded checksum
    // (a `<hex>  <binary-name>` line inside the `.sha256` sidecar, when
    // published). Falls back to archive-checksum + smoke test otherwise.
    match expected_binary_sha(checksum, exe_name).await {
        Ok(Some(hex)) => {
            if let Err(e) = verify_file_sha256(&exe, &hex) {
                rollback_restore(&exe, &bak);
                return Err(anyhow::anyhow!(
                    "installed binary failed sha256 verification: {e}; previous version restored (backup kept at {})",
                    bak.display()
                ));
            }
        }
        Ok(None) => {
            eprintln!(
                "warning: release does not publish a binary-level sha256; relying on archive checksum + smoke test"
            );
        }
        Err(e) => {
            eprintln!(
                "warning: could not fetch the binary-level checksum ({e}); relying on archive checksum + smoke test"
            );
        }
    }

    // Smoke test: the new binary must report the target version.
    if !smoke_test_version(&exe, &check.latest_version.to_string()) {
        rollback_restore(&exe, &bak);
        return Err(anyhow::anyhow!(
            "new binary failed the smoke test (--version did not report v{}); previous version restored (backup kept at {})",
            check.latest_version,
            bak.display()
        ));
    }

    println!("done. Upgraded review-engine to v{}.", check.latest_version);
    println!(
        "Previous binary kept at {}; roll back with `reng upgrade --rollback`.",
        bak.display()
    );
    Ok(())
}

/// Expected sha256 of the extracted binary: re-fetch the release's `.sha256`
/// sidecar and look for a `<hex>  <binary-name>` line. `None` means the
/// release does not publish a binary-level checksum.
async fn expected_binary_sha(checksum: &ReleaseAsset, binary_name: &str) -> Result<Option<String>> {
    let tmp = unique_temp_dir(&std::env::temp_dir(), "checksum")?;
    let _cleanup = CleanupPaths(vec![tmp.clone()]);
    let (sidecar_path, _) = download_asset(&checksum.download_url, &tmp, &checksum.name, Some(checksum.size)).await?;
    let text = std::fs::read_to_string(&sidecar_path)?;
    Ok(parse_sidecar_binary_hex(&text, binary_name))
}

fn parse_sidecar_binary_hex(text: &str, binary_name: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Ok((hex, name)) = parse_sha256_line(line) {
            if name == binary_name {
                return Some(hex);
            }
        }
    }
    None
}

/// Locate `binary_name` in the extracted tree, preferring the shallowest
/// match (e.g. `bin/review-engine` over a nested copy).
fn find_binary_in(root: &Path, binary_name: &str) -> Option<PathBuf> {
    let mut best: Option<(usize, PathBuf)> = None;
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.is_file() && path.file_name().and_then(|n| n.to_str()) == Some(binary_name) {
                let depth = path.components().count();
                if best.as_ref().map(|(d, _)| depth < *d).unwrap_or(true) {
                    best = Some((depth, path));
                }
            }
        }
    }
    best.map(|(_, p)| p)
}

/// Move the extracted binary into place, falling back to copy+remove for a
/// cross-device rename, then mark it executable.
fn install_binary(from: &Path, to: &Path) -> std::io::Result<()> {
    if let Err(rename_err) = std::fs::rename(from, to) {
        std::fs::copy(from, to).map_err(|copy_err| {
            std::io::Error::new(
                rename_err.kind(),
                format!("rename failed ({rename_err}); copy fallback failed ({copy_err})"),
            )
        })?;
        let _ = std::fs::remove_file(from);
    }
    set_executable(to)
}

fn set_executable(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)?.permissions();
        perms.set_mode(perms.mode() | 0o111);
        std::fs::set_permissions(path, perms)
    }
    #[cfg(not(unix))]
    {
        Ok(())
    }
}

/// Restore the previous binary after a failed install. Copies (rather than
/// renames) so the `.bak` survives for a later explicit `--rollback`.
fn rollback_restore(exe: &Path, bak: &Path) {
    let _ = std::fs::remove_file(exe);
    if let Err(e) = std::fs::copy(bak, exe) {
        eprintln!("warning: failed to restore the previous binary: {e}");
    }
    let _ = set_executable(exe);
}

/// Run `<exe> --version`; it must succeed and print the target version.
fn smoke_test_version(exe: &Path, target: &str) -> bool {
    match std::process::Command::new(exe).arg("--version").output() {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            out.status.success() && (stdout.contains(target) || stderr.contains(target))
        }
        Err(_) => false,
    }
}

fn unique_temp_dir(base: &Path, tag: &str) -> Result<PathBuf> {
    let nonce: u64 = rand::random();
    let dir = base.join(format!(".review-engine-{tag}-{}-{:x}", std::process::id(), nonce));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Best-effort removal of temp files/dirs on drop (success or error).
struct CleanupPaths(Vec<PathBuf>);

impl Drop for CleanupPaths {
    fn drop(&mut self) {
        for p in &self.0 {
            let _ = std::fs::remove_dir_all(p);
            let _ = std::fs::remove_file(p);
        }
    }
}

/// Exclusive lock preventing two upgrades of the same directory at once.
/// `create_new`, contains `pid=<pid> ts=<unix-seconds>`, removed on drop.
/// A lock older than `LOCK_STALE_SECS` is treated as stale and reclaimed.
#[derive(Debug)]
struct UpgradeLock {
    path: PathBuf,
}

impl UpgradeLock {
    fn acquire(dir: &Path) -> Result<UpgradeLock> {
        use std::io::Write;
        let path = dir.join(LOCK_FILE_NAME);
        let now = unix_now_secs();
        for attempt in 0..2 {
            let content = format!("pid={} ts={}\n", std::process::id(), now);
            match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    file.write_all(content.as_bytes())?;
                    return Ok(UpgradeLock { path });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let stale = read_lock_ts(&path)
                        .map(|ts| now.saturating_sub(ts) > LOCK_STALE_SECS)
                        .unwrap_or(false);
                    if stale {
                        let _ = std::fs::remove_file(&path);
                        if attempt == 0 {
                            continue;
                        }
                    }
                    let pid = read_lock_pid(&path)
                        .map(|p| p.to_string())
                        .unwrap_or_else(|| "unknown".to_string());
                    anyhow::bail!(
                        "another upgrade appears to be in progress (lock: {}, pid={}); remove the lock file if it is stale",
                        path.display(),
                        pid
                    );
                }
                Err(e) => return Err(e.into()),
            }
        }
        anyhow::bail!("could not acquire the upgrade lock at {}", path.display())
    }
}

impl Drop for UpgradeLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn unix_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn read_lock_pid(path: &Path) -> Option<u64> {
    let text = std::fs::read_to_string(path).ok()?;
    text.split_whitespace()
        .find_map(|tok| tok.strip_prefix("pid="))
        .and_then(|v| v.parse().ok())
}

fn read_lock_ts(path: &Path) -> Option<u64> {
    let text = std::fs::read_to_string(path).ok()?;
    text.split_whitespace()
        .find_map(|tok| tok.strip_prefix("ts="))
        .and_then(|v| v.parse().ok())
}

#[cfg(test)]
mod upgrade_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn install_source_labels_are_stable() {
        assert_eq!(install_source_label(InstallMethod::Brew), "Homebrew");
        assert_eq!(install_source_label(InstallMethod::Cargo), "Cargo (~/.cargo/bin)");
        assert_eq!(install_source_label(InstallMethod::Docker), "Docker 容器");
        assert_eq!(install_source_label(InstallMethod::Plain), "直接部署的二进制");
        assert_eq!(install_source_label(InstallMethod::Unknown), "未知（手动安装）");
    }

    #[test]
    fn parses_binary_hex_from_sidecar() {
        let text = format!(
            "{}  review-engine-aarch64-apple-darwin.tar.gz\n{}  review-engine\n",
            "a".repeat(64),
            "b".repeat(64)
        );
        assert_eq!(parse_sidecar_binary_hex(&text, "review-engine"), Some("b".repeat(64)));
        assert_eq!(parse_sidecar_binary_hex(&text, "review-engine.exe"), None);
        assert_eq!(parse_sidecar_binary_hex("# only comments\n", "review-engine"), None);
    }

    #[test]
    fn lock_conflicts_and_releases() {
        let dir = tempdir().unwrap();
        let lock = UpgradeLock::acquire(dir.path()).unwrap();
        let path = dir.path().join(LOCK_FILE_NAME);
        assert!(path.exists(), "lock file must exist while held");

        let err = UpgradeLock::acquire(dir.path()).unwrap_err();
        assert!(err.to_string().contains("in progress"), "got: {err}");

        drop(lock);
        assert!(!path.exists(), "lock file must be removed on drop");

        let lock2 = UpgradeLock::acquire(dir.path()).unwrap();
        drop(lock2);
    }

    #[test]
    fn stale_lock_is_reclaimed() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(LOCK_FILE_NAME);
        let old_ts = unix_now_secs().saturating_sub(LOCK_STALE_SECS + 60);
        std::fs::write(&path, format!("pid=1 ts={old_ts}\n")).unwrap();
        let lock = UpgradeLock::acquire(dir.path()).unwrap();
        drop(lock);
        assert!(!path.exists(), "stale lock must be reclaimed and removed");
    }

    #[test]
    fn finds_binary_in_extracted_tree() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("out");
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::write(root.join("LICENSE"), "license").unwrap();
        let target = root.join("bin").join("review-engine");
        std::fs::write(&target, "#!/bin/sh").unwrap();
        std::fs::write(root.join("bin").join("other"), "x").unwrap();
        assert_eq!(find_binary_in(&root, "review-engine"), Some(target));
        assert_eq!(find_binary_in(&root, "review-engine.exe"), None);
    }

    #[cfg(unix)]
    #[test]
    fn canonical_exe_path_resolves_symlink() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let real = dir.path().join("review-engine");
        std::fs::write(&real, "#!/bin/sh").unwrap();
        let link = dir.path().join("reng");
        symlink(&real, &link).unwrap();
        // macOS tempdirs live under /var/folders which is a symlink to
        // /private/var/folders, so compare against the canonicalized real path.
        let real_canonical = std::fs::canonicalize(&real).unwrap();

        // A symlink invocation path (what macOS current_exe() returns) must
        // resolve to the real binary, not stay as the link.
        assert_eq!(canonical_exe_path(&link), real_canonical);
        // A real path is returned as its canonical form.
        assert_eq!(canonical_exe_path(&real), real_canonical);
        // A missing path falls back to the raw value.
        let missing = dir.path().join("nope");
        assert_eq!(canonical_exe_path(&missing), missing);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use review_engine::team::lead_consolidator::{ConsolidatedReport, ExpertConflict};

    fn make_finding(severity: Severity, file: &str) -> Finding {
        Finding {
            file: file.to_string(),
            line: Some(42),
            line_end: None,
            severity,
            confidence: 8,
            category: String::new(),
            title: "Test finding".to_string(),
            summary: "Detail".to_string(),
            evidence: String::new(),
            impact: String::new(),
            recommendation: "Fix it".to_string(),
            effort: Effort::Small,
            expert_name: "security".to_string(),
            expert_role: "Security".to_string(),
            agrees_with: vec![],
            references: vec![],
        }
    }

    fn make_consolidated() -> ConsolidatedReport {
        ConsolidatedReport {
            findings: vec![],
            low_confidence_removed: 0,
            duplicates_merged: 1,
            conflicts: vec![ExpertConflict {
                file: "src/auth.rs".to_string(),
                line: Some(10),
                issue: "Token comparison".to_string(),
                experts: vec!["security".to_string(), "performance".to_string()],
                resolutions: vec![
                    "Use constant-time comparison".to_string(),
                    "Cache the token hash".to_string(),
                ],
            }],
            assessment: OverallAssessment {
                score: 72,
                risk_level: RiskLevel::Medium,
                lead_override: None,
                tl_dr: "Risk Level: Medium. 1 high found by 2 reviewers.".to_string(),
            },
            consensus_reached: true,
            total_files: 0,
            reviewed_files: 0,
            unreviewed_files: vec![],
        }
    }

    fn sample_output(consolidated: Option<ConsolidatedReport>) -> ReviewOutput {
        ReviewOutput {
            reports: vec![ExpertReport {
                expert_name: "security".to_string(),
                findings: vec![make_finding(Severity::High, "src/main.rs")],
                markdown: "## Security Review\n\nSome findings.\n".to_string(),
                raw_llm_response: String::new(),
            }],
            aggregated: None,
            dropped_findings: vec![],
            consolidated,
        }
    }

    #[test]
    fn test_prepare_review_carries_real_local_path() {
        // Root cause C: `prepare_review` must carry the real `--local-path`
        // into MRInfo.project_path (not the "local" placeholder), so the
        // full-file contents injection reads from the actual checkout.
        let config = AppConfig {
            project: None,
            report: Default::default(),
            review_experts: Default::default(),
            commands: Default::default(),
            scoring: Default::default(),
            llm: vec![],
            max_team_size: None,
            max_concurrent_llm_calls: None,
            output_dir: String::new(),
            diff: Default::default(),
            rate_limit: Default::default(),
            languages: Default::default(),
        };
        let (_, mr_info) = prepare_review(&config, "/real/repo", "local", "main");
        assert_eq!(mr_info.project_path, "/real/repo");
    }
    fn render(result: &ReviewOutput, format: &str, verification_enabled: bool) -> String {
        match format_output(result, format, verification_enabled) {
            Ok(s) => s,
            Err(e) => panic!("format_output failed: {}", e),
        }
    }

    #[test]
    fn test_format_output_markdown_includes_lead_summary() {
        let out = render(&sample_output(Some(make_consolidated())), "markdown", false);
        assert!(out.contains("## Security Review"));
        assert!(out.contains("## Lead Summary"));
        assert!(out.contains("Overall Score: **72/100**"));
        assert!(out.contains("Risk Level: medium"));
        assert!(out.contains("### TL;DR"));
        assert!(out.contains("1 high found by 2 reviewers"));
        assert!(out.contains("### ⚖️ Reviewer Discussion"));
        assert!(out.contains("`src/auth.rs:10`"));
        // Lead Summary renders after the expert report.
        let expert_pos = out.find("## Security Review");
        let lead_pos = out.find("## Lead Summary");
        assert!(expert_pos < lead_pos);
    }

    #[test]
    fn test_format_output_markdown_lead_summary_before_appendix() {
        let mut output = sample_output(Some(make_consolidated()));
        output
            .dropped_findings
            .push(review_engine::team::verifier::DroppedFinding {
                finding: make_finding(Severity::Medium, "src/lib.rs"),
                reason: "Not in diff".to_string(),
            });
        let out = render(&output, "markdown", true);
        let lead_pos = out.find("## Lead Summary");
        let appendix_pos = out.find("## Dropped by verification");
        assert!(lead_pos < appendix_pos);
    }

    #[test]
    fn test_format_output_markdown_without_consolidated_unchanged() {
        let out = render(&sample_output(None), "markdown", false);
        assert!(out.contains("## Security Review"));
        assert!(!out.contains("Lead Summary"));
    }

    #[test]
    fn test_format_output_json_has_consolidated_field() {
        let out = render(&sample_output(Some(make_consolidated())), "json", false);
        assert!(out.contains("\"consolidated\""));
        assert!(out.contains("\"score\": 72"));
        assert!(out.contains("\"risk_level\": \"Medium\""));
    }
}
