use anyhow::Result;
use review_engine::models::*;
use review_engine::progress::ProgressMap;
use std::path::Path;

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
    if let Ok(json) = std::env::var("LLM_CONFIG") {
        if !json.is_empty() && json != "[]" {
            return Ok(serde_json::from_str(&json)?);
        }
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

pub async fn run_ask_local_diff(
    question: &str,
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
        "Local ask".to_string(),
        "local".to_string(),
        "main".to_string(),
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

    run_ask_local_diff(&diff, question, config_path, llm_configs, format, output).await
}

pub async fn run_local(
    diff_path: &str,
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

    let (experts, mr_info) = prepare_review(&config, "local", "local", "main");

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
        assert!(out.contains("### Expert Conflicts"));
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
