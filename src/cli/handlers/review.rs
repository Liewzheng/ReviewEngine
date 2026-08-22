use anyhow::Result;
use review_engine::models::*;
use review_engine::progress::ProgressMap;
use std::path::PathBuf;

use super::output::{write_output, write_report_text};

/// Resolve the `--verbose` raw-dump directory: `<output>.raw/` when an explicit
/// `--output` file is given, otherwise `<output_dir>/review-raw/`. Returns
/// `None` when `--verbose` is off or the directory cannot be created (a
/// warning is printed; the review still runs).
fn verbose_dump_dir(verbose: bool, output: &Option<String>, output_dir: &str) -> Option<PathBuf> {
    if !verbose {
        return None;
    }
    let dir = match output {
        Some(path) => PathBuf::from(format!("{path}.raw")),
        None => PathBuf::from(output_dir).join("review-raw"),
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("warning: [verbose] failed to create dump dir {}: {e}", dir.display());
        return None;
    }
    Some(dir)
}

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

/// True when at least one config entry is usable: a non-empty `api_base`
/// (`api_key` may stay empty — local providers need no key).
pub(crate) fn has_usable_llm(configs: &[LLMConfig]) -> bool {
    configs.iter().any(|c| !c.api_base.trim().is_empty())
}

/// The user-level config file path, resolved with the same home-dir logic as
/// `config::resolver::user_fallback`
/// (`~/.config/review-engine/.code-audit-config.toml`).
fn user_config_path_display() -> String {
    home::home_dir()
        .map(|p| p.join(".config").join("review-engine").join(".code-audit-config.toml"))
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "~/.config/review-engine/.code-audit-config.toml".to_string())
}

/// The fail-fast guidance error shown when no usable LLM is configured.
pub(crate) fn no_usable_llm_error() -> anyhow::Error {
    anyhow::anyhow!(
        "no usable LLM configured — reviews require an LLM.\n\
         \n\
         Configure one (any of):\n\
         \x20 1. Run the interactive wizard:  review-engine init\n\
         \x20 2. Add an [[llm]] section to your config file (provider, model, api_base, api_key):\n\
         \x20    {}\n\
         \x20 3. Set the LLM_CONFIG env var, e.g.:\n\
         \x20    LLM_CONFIG='[{{\"provider\":\"openai\",\"model\":\"gpt-4o\",\"api_base\":\"https://api.openai.com/v1\",\"api_key\":\"sk-...\"}}]'",
        user_config_path_display()
    )
}

/// Fail fast with configuration guidance when nothing usable is configured
/// (empty list, or no entry with a non-empty `api_base`).
pub(crate) fn ensure_usable_llm(configs: &[LLMConfig]) -> anyhow::Result<()> {
    if has_usable_llm(configs) {
        Ok(())
    } else {
        Err(no_usable_llm_error())
    }
}

/// Resolve LLM configs, then fail fast with configuration guidance when no
/// usable LLM is configured — instead of failing deep in the pipeline with
/// "all LLM providers failed".
pub fn require_llm_configs(argv_llm_configs: &[String], config: &AppConfig) -> anyhow::Result<Vec<LLMConfig>> {
    let configs = resolve_llm_configs(argv_llm_configs, config)?;
    ensure_usable_llm(&configs)?;
    Ok(configs)
}

pub(super) fn is_github_url(url: &str) -> bool {
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

    let result = review_engine::run_review(
        mr_url,
        token,
        llm_configs,
        config_toml.map(ConfigSource::Inline),
        None,
        None,
    )
    .await?;
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
    verbose: bool,
) -> Result<()> {
    let token = if is_github_url(mr_url) {
        github_token.unwrap_or_else(|| std::env::var("GITHUB_TOKEN").unwrap_or_default())
    } else {
        gitlab_token.unwrap_or_else(|| std::env::var("GITLAB_TOKEN").unwrap_or_default())
    };
    let config_source = config_path.map(ConfigSource::Path);
    let config = review_engine::config::resolve_config(config_source.clone()).await?;
    let configs: Vec<LLMConfig> = require_llm_configs(&llm_configs, &config)?;
    let dump_dir = verbose_dump_dir(verbose, output, &config.output_dir);

    let progress_override = progress_map.map(|map| (map, review_id.to_string()));
    let result = review_engine::run_review(mr_url, &token, configs, config_source, progress_override, dump_dir).await?;
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

pub async fn run_local(
    diff_path: &str,
    local_path: Option<&str>,
    config_path: Option<String>,
    llm_configs: Vec<String>,
    format: &str,
    output: &Option<String>,
    progress_map: Option<ProgressMap>,
    review_id: &str,
    verbose: bool,
) -> Result<()> {
    let diff = tokio::fs::read_to_string(diff_path).await?;
    let config_source = config_path.map(ConfigSource::Path);
    let config = review_engine::config::resolve_config(config_source).await?;
    let llm_configs: Vec<LLMConfig> = require_llm_configs(&llm_configs, &config)?;
    let dump_dir = verbose_dump_dir(verbose, output, &config.output_dir);

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
        dump_dir,
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

pub(super) fn prepare_review(
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
    verbose: bool,
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

    ensure_usable_llm(&llm_configs)?;
    let dump_dir = verbose_dump_dir(verbose, output, &config.output_dir);

    let (experts, mr_info) = prepare_review(&config, local_path, "local", base_ref);

    let (reports, _, dropped_findings, consolidated) = review_engine::team::orchestrator::run_experts(
        &experts,
        &mr_info,
        &diff,
        &llm_configs,
        &config,
        progress_map.clone(),
        review_id,
        dump_dir,
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
    verbose: bool,
) -> Result<()> {
    let config_source = config_path.map(ConfigSource::Path);
    let config = review_engine::config::resolve_config(config_source).await?;
    let llm_configs: Vec<LLMConfig> = resolve_llm_configs(&llm_configs, &config)?;

    // Validate the review input FIRST: a bad --path (missing directory, empty
    // tree, traversal) must fail with the path error regardless of LLM
    // configuration. The LLM check below otherwise masks it with an unrelated
    // "no usable LLM configured" message in environments that resolve no
    // provider (e.g. clean CI) — and the actionable error is the path one.
    let full = review_engine::input::full_path::build_path_review_diff(local_path, path)?;
    let file_count = full.files.len();

    ensure_usable_llm(&llm_configs)?;
    let dump_dir = verbose_dump_dir(verbose, output, &config.output_dir);

    let (experts, mr_info) = prepare_review(&config, local_path, "local", "main");

    let (reports, _, dropped_findings, consolidated) = review_engine::team::orchestrator::run_experts(
        &experts,
        &mr_info,
        &full.diff,
        &llm_configs,
        &config,
        progress_map.clone(),
        review_id,
        dump_dir,
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
            parse_error: None,
            raw_dump_path: None,
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
    // Captured before `config` is shadowed by the Arc below: the unified
    // output sink double-writes a timestamped copy into the reports dir.
    let output_dir = config.output_dir.clone();
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
    // Unified sink, same as the MR review paths: no --output prints to stdout
    // AND saves a timestamped copy into the reports dir (default runs land on
    // disk); --output writes the explicit file plus the same timestamped copy.
    write_report_text(&text, format, output, Some(&output_dir))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn llm_config(api_base: &str) -> LLMConfig {
        LLMConfig {
            provider: "openai".to_string(),
            model: "gpt-4o".to_string(),
            api_key: String::new(),
            api_base: api_base.to_string(),
            max_tokens: 4096,
            temperature: 0.7,
            disable_thinking: None,
        }
    }

    fn empty_app_config() -> AppConfig {
        toml::from_str("").expect("empty TOML must parse into AppConfig")
    }

    #[test]
    fn ensure_usable_llm_rejects_empty_configs() {
        let err = ensure_usable_llm(&[]).expect_err("empty configs must fail");
        let msg = err.to_string();
        assert!(msg.contains("no usable LLM configured"), "got: {msg}");
        assert!(msg.contains("review-engine init"), "got: {msg}");
        assert!(msg.contains(".code-audit-config.toml"), "got: {msg}");
        assert!(msg.contains("LLM_CONFIG"), "got: {msg}");
    }

    #[test]
    fn ensure_usable_llm_rejects_entries_without_api_base() {
        let configs = vec![llm_config(""), llm_config("   ")];
        let err = ensure_usable_llm(&configs).expect_err("entries without api_base must fail");
        assert!(err.to_string().contains("no usable LLM configured"));
    }

    #[test]
    fn ensure_usable_llm_accepts_config_with_api_base() {
        // api_key may stay empty — local providers need no key.
        let configs = vec![llm_config("http://localhost:11434/v1")];
        assert!(ensure_usable_llm(&configs).is_ok());
    }

    #[test]
    fn require_llm_configs_gates_argv_configs() {
        // argv-provided configs bypass env/config-file resolution, so this is
        // deterministic without mutating the LLM_CONFIG env var.
        let config = empty_app_config();

        let no_base = vec![serde_json::to_string(&llm_config("")).unwrap()];
        let err = require_llm_configs(&no_base, &config).expect_err("argv configs without api_base must fail");
        assert!(err.to_string().contains("no usable LLM configured"));

        let usable = vec![serde_json::to_string(&llm_config("http://localhost:11434/v1")).unwrap()];
        let resolved = require_llm_configs(&usable, &config).expect("usable argv config must pass the gate");
        assert_eq!(resolved.len(), 1);
    }
}
