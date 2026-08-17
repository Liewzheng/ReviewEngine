use anyhow::Result;
use review_engine::models::*;

use super::output::write_output;
use super::review::resolve_llm_configs;

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
