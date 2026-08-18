//! `review-engine init` — interactive project initialization.
//!
//! Scans the repository, detects languages / CI / test frameworks, prompts
//! the user for preferences (commands, experts, LLM), and writes the
//! resulting `.code-audit-config.toml` to disk.
//!
//! The LLM section is backed by the models.dev provider catalog
//! ([`crate::catalog`]): providers and models are picked from live data, and
//! the flow degrades to a manual DeepSeek template when the catalog is
//! unreachable.

use anyhow::Result;
use inquire::{Confirm, MultiSelect, Select, Text};

use crate::catalog::{self, Catalog, CatalogClient, CatalogModel, CatalogProvider, CatalogSource};
use crate::config::defaults::default_config;
use crate::repo::RepoScanner;

/// Available experts with their default weights, roles, and descriptions.
const AVAILABLE_EXPERTS: &[(&str, u8, &str, &str)] = &[
    ("lead", 20, "Lead Reviewer", "overall assessment & quality gate"),
    ("security", 15, "Security Lead", "vulnerability & threat analysis"),
    ("performance", 10, "Performance Lead", "efficiency & scalability"),
    ("quality", 10, "Quality Lead", "test coverage & edge cases"),
    ("reuse", 12, "Reuse Lead", "code duplication & refactoring"),
    ("docs", 5, "Docs Lead", "documentation & changelog"),
    ("ux", 8, "User Interface Expert", "naming, ergonomics, human factors"),
    ("database", 5, "Database Expert", "schema & query performance"),
    ("devops", 5, "DevOps Expert", "CI/CD, infra, secrets"),
    ("api", 5, "API Design Expert", "contracts & backward compatibility"),
    ("dependency", 5, "Dependency Expert", "supply chain & licenses"),
];

const AVAILABLE_COMMANDS: &[(&str, &str)] = &[
    ("review", "MR/PR code review"),
    ("repo_review", "full repo health check"),
    ("improve", "code improvement suggestions"),
    ("describe", "PR description / summary"),
    ("ask", "free-form Q&A about the diff"),
    ("update_changelog", "CHANGELOG generation"),
];

/// Print a section header.
fn section(title: &str) {
    println!();
    println!("  {}", "─".repeat(40));
    println!("  {title}");
    println!("  {}", "─".repeat(40));
    println!();
}

fn print_header(lang: &str, has_ci: bool, has_test: bool, file_count: usize, loc: usize) {
    println!();
    println!("  {}", "─".repeat(40));
    println!("  review-engine 项目初始化");
    println!("  {}", "─".repeat(40));
    println!("  Language:     {lang}");
    println!("  Files:        {file_count}");
    println!("  LOC:          {loc}");
    println!("  CI:           {}", if has_ci { "detected" } else { "not found" });
    println!("  Test:         {}", if has_test { "detected" } else { "not found" });
    println!("  {}", "─".repeat(40));
    println!();
}

/// Build an inquire-compatible display string for a command entry.
fn fmt_cmd((name, desc): &(&str, &str)) -> String {
    format!("  {name:<15}  {desc}")
}

/// Build an inquire-compatible display string for an expert entry.
fn fmt_expert((name, weight, _role, desc): &(&str, u8, &str, &str)) -> String {
    format!("  {name:<12}  weight {weight:<3}  {desc}")
}

/// Result of scanning the project repository.
struct ScanResult {
    dominant: String,
    has_ci: bool,
    has_test: bool,
    total_files: usize,
    total_loc: usize,
}

/// Scan the project repository and return detected metadata.
fn scan_project(local_path: &str) -> Result<ScanResult> {
    let scanner = RepoScanner::new(local_path);
    let entries = scanner.scan()?;
    let stats = scanner.compute_stats(&entries);

    let dominant = stats
        .languages
        .iter()
        .max_by_key(|(_, s)| s.files)
        .map(|(n, _)| n.clone())
        .unwrap_or_else(|| "Unknown".to_string());

    let has_ci = entries
        .iter()
        .any(|e| e.path.contains(".gitlab-ci.yml") || e.path.contains(".github/workflows/"));

    let has_test = entries.iter().any(|e| {
        e.path.contains("Cargo.toml")
            && std::fs::read_to_string(&e.path)
                .ok()
                .map_or(false, |c| c.contains("[dev-dependencies]"))
    }) || entries
        .iter()
        .any(|e| e.path.contains("pyproject.toml") || e.path.contains("package.json"));

    Ok(ScanResult {
        dominant,
        has_ci,
        has_test,
        total_files: stats.total_files,
        total_loc: stats.total_loc,
    })
}

/// Prompt the user to select which commands to enable.
fn prompt_commands() -> Result<Vec<usize>> {
    section("Commands");
    let cmd_displays: Vec<String> = AVAILABLE_COMMANDS.iter().map(fmt_cmd).collect();
    let cmd_selected = MultiSelect::new("Enable commands", cmd_displays.clone())
        .with_default(&[0, 1])
        .with_formatter(&|_| String::new())
        .prompt()?;
    let cmd_indices: Vec<usize> = cmd_selected
        .iter()
        .filter_map(|s| cmd_displays.iter().position(|d| d == s))
        .collect();
    if !cmd_indices.is_empty() {
        println!();
        for &i in &cmd_indices {
            let (name, desc) = AVAILABLE_COMMANDS[i];
            println!("    \u{2022} {name} \u{2014} {desc}");
        }
        println!();
    }
    Ok(cmd_indices)
}

/// Provider ids shown as a curated shortlist before the full catalog list.
/// Order matters: this is the order the shortlist displays.
const CURATED_PROVIDER_IDS: &[&str] = &[
    "openai",
    "anthropic",
    "deepseek",
    "google",
    "xai",
    "mistralai",
    "groq",
    "openrouter",
    "ollama",
];

/// Prompt the user for LLM configuration.
async fn prompt_llm() -> Result<LlmPromptOutcome> {
    section("LLM (AI Review)");
    let enable_llm = Confirm::new("Enable LLM-based AI review? (skip for local-only static analysis)")
        .with_default(true)
        .prompt()?;

    if !enable_llm {
        return Ok(LlmPromptOutcome::new(String::new(), "disabled".to_string()));
    }

    println!("  Fetching provider catalog from models.dev…");
    match fetch_catalog().await {
        Some((catalog, source)) => {
            if let CatalogSource::DiskCache(fetched_at) = source {
                println!(
                    "  (models.dev unreachable — using catalog cached at {})",
                    fetched_at.format("%Y-%m-%d %H:%M UTC")
                );
            }
            let providers = catalog::usable_providers(&catalog);
            if providers.is_empty() {
                println!("  Provider catalog carried no usable providers; falling back to manual configuration.");
                prompt_llm_fallback()
            } else {
                prompt_llm_from_catalog(&providers)
            }
        }
        None => {
            println!("  Provider catalog unavailable; falling back to manual configuration.");
            prompt_llm_fallback()
        }
    }
}

/// Fetch the models.dev catalog, honoring the disk-cache fallback. Returns
/// `None` when both the network and the disk cache fail.
async fn fetch_catalog() -> Option<(Catalog, CatalogSource)> {
    let client = CatalogClient::from_env()
        .map_err(|e| tracing::warn!("Catalog client init failed: {e}"))
        .ok()?;
    let cache_path = catalog::default_cache_path();
    catalog::fetch_or_disk_fallback(&client, cache_path.as_deref())
        .await
        .map_err(|e| tracing::warn!("Catalog fetch failed: {e}"))
        .ok()
}

/// The curated shortlist, in [`CURATED_PROVIDER_IDS`] order, restricted to
/// providers actually present in the catalog.
fn curated_shortlist<'a>(providers: &[&'a CatalogProvider]) -> Vec<&'a CatalogProvider> {
    CURATED_PROVIDER_IDS
        .iter()
        .filter_map(|id| providers.iter().find(|p| p.id == *id).copied())
        .collect()
}

/// Display string for a provider entry in an inquire `Select`.
fn provider_display(p: &CatalogProvider) -> String {
    format!("  {} ({})", p.name, p.id)
}

/// Display string for a model entry: name, id, and context limit.
fn model_display(m: &CatalogModel) -> String {
    let context = m.limit.as_ref().and_then(|l| l.context);
    format!("  {} ({}) — {}", m.name, m.id, fmt_context(context))
}

/// Human-friendly context limit for the model picker (`128000` → `128k ctx`).
fn fmt_context(context: Option<u64>) -> String {
    match context {
        Some(n) if n >= 1000 => format!("{}k ctx", n / 1000),
        Some(n) => format!("{n} ctx"),
        None => "unknown context".to_string(),
    }
}

/// Render the `[[llm]]` TOML block for the chosen provider/model.
///
/// With no API key the `api_key` line is commented out, so the file parses
/// while making the missing credential explicit.
fn render_llm_block(provider: &str, model: &str, api_base: &str, api_key: Option<&str>) -> String {
    let key_line = match api_key {
        Some(key) => format!("api_key = \"{key}\"\n"),
        None => "# api_key = \"...\"  # fill in, or pass via LLM_CONFIG env / --llm-config\n".to_string(),
    };
    format!(
        "[[llm]]\nprovider = \"{provider}\"\nmodel = \"{model}\"\n{key_line}api_base = \"{api_base}\"\nmax_tokens = 4096\ntemperature = 0.3\n\n"
    )
}

/// Outcome of the LLM prompt section: the rendered TOML block, the summary
/// note, and hints printed after the config file is written.
struct LlmPromptOutcome {
    /// The `[[llm]]` TOML block (empty when LLM is disabled or deferred).
    block: String,
    /// One-line summary shown in the configuration summary.
    note: String,
    /// Hints printed after the config file is written (e.g. plaintext-key
    /// warning).
    post_init_hints: Vec<String>,
}

impl LlmPromptOutcome {
    fn new(block: String, note: String) -> Self {
        Self {
            block,
            note,
            post_init_hints: Vec::new(),
        }
    }
}

/// Where an API key entered the init flow from.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ApiKeySource {
    /// Read from a provider environment variable (e.g. `DEEPSEEK_API_KEY`)
    /// after the user confirmed. Carries the variable's name.
    Env(String),
    /// Typed in at the prompt.
    Manual,
}

/// Post-init hint shown when an env-var key was written to the config file
/// in plaintext. review-engine does not resolve per-provider env vars (e.g.
/// `DEEPSEEK_API_KEY`) into `[[llm]]` entries at runtime — only the
/// `LLM_CONFIG` JSON env var and `--llm-config` supply credentials outside
/// the config file — so the key must be written for the config to work, but
/// shared machines should prefer the runtime channels.
fn plaintext_key_hint(env_var: &str) -> String {
    format!(
        "The API key from {env_var} was written to the config file in plaintext. \
         On shared machines, prefer removing the api_key line and passing credentials \
         via the LLM_CONFIG env var or --llm-config at runtime."
    )
}

/// One-line configuration-summary note for the chosen provider/model.
fn llm_note(provider_name: &str, model: &str, api_key: &Option<(String, ApiKeySource)>) -> String {
    match api_key {
        Some((_, ApiKeySource::Env(var))) => format!("{provider_name} / {model} (key from {var})"),
        Some(_) => format!("{provider_name} / {model}"),
        None => format!("{provider_name} / {model} (API key pending)"),
    }
}

/// Catalog-backed interactive flow: pick provider → model → API key, then
/// render the `[[llm]]` block.
fn prompt_llm_from_catalog(providers: &[&CatalogProvider]) -> Result<LlmPromptOutcome> {
    let provider = select_catalog_provider(providers)?;

    let models = catalog::sorted_models(provider);
    let model = if models.is_empty() {
        Text::new("  Model id").prompt()?.trim().to_string()
    } else {
        select_catalog_model(&models)?
    };
    if model.is_empty() {
        anyhow::bail!("A model id is required");
    }

    let api_base = catalog::normalize_api_base(provider.npm.as_deref(), provider.api.as_deref().unwrap_or_default());
    let api_key = prompt_api_key(provider.env.first().map(String::as_str))?;

    let block = render_llm_block(
        &provider.id,
        &model,
        &api_base,
        api_key.as_ref().map(|(key, _)| key.as_str()),
    );
    let mut outcome = LlmPromptOutcome::new(block, llm_note(&provider.name, &model, &api_key));
    if let Some((_, ApiKeySource::Env(var))) = &api_key {
        outcome.post_init_hints.push(plaintext_key_hint(var));
    }
    Ok(outcome)
}

/// Pick a provider: curated shortlist first, "Browse all…" escapes to the
/// full (filterable) list.
fn select_catalog_provider<'a>(providers: &[&'a CatalogProvider]) -> Result<&'a CatalogProvider> {
    let shortlist = curated_shortlist(providers);
    let browse = format!("  Browse all {} providers…", providers.len());

    if shortlist.is_empty() {
        return browse_all_providers(providers, &browse);
    }

    let mut displays: Vec<String> = shortlist.iter().map(|p| provider_display(p)).collect();
    displays.push(browse.clone());
    let choice = Select::new("LLM provider", displays).prompt()?;

    if choice == browse {
        browse_all_providers(providers, &browse)
    } else {
        let idx = shortlist
            .iter()
            .map(|p| provider_display(p))
            .position(|d| d == choice)
            .ok_or_else(|| anyhow::anyhow!("provider selection lost"))?;
        Ok(shortlist[idx])
    }
}

/// Full-catalog provider picker. inquire's `Select` filters as the user
/// types; the page size keeps the list scrollable.
fn browse_all_providers<'a>(providers: &[&'a CatalogProvider], message: &str) -> Result<&'a CatalogProvider> {
    let all: Vec<String> = providers.iter().map(|p| provider_display(p)).collect();
    let pick = Select::new(message, all.clone()).with_page_size(20).prompt()?;
    let idx = all
        .iter()
        .position(|d| d == &pick)
        .ok_or_else(|| anyhow::anyhow!("provider selection lost"))?;
    Ok(providers[idx])
}

/// Pick a model from the chosen provider's catalog entries.
fn select_catalog_model(models: &[&CatalogModel]) -> Result<String> {
    let displays: Vec<String> = models.iter().map(|m| model_display(m)).collect();
    let pick = Select::new("Model", displays.clone()).with_page_size(20).prompt()?;
    let idx = displays
        .iter()
        .position(|d| d == &pick)
        .ok_or_else(|| anyhow::anyhow!("model selection lost"))?;
    Ok(models[idx].id.clone())
}

/// Resolve the API key: offer the provider's canonical env var when set
/// (never printing the value), otherwise free-text entry. `None` means the
/// user deferred credentials to runtime. The returned [`ApiKeySource`]
/// records whether the value came from the environment, so the caller can
/// warn when a plaintext env key lands in the config file.
fn prompt_api_key(env_var: Option<&str>) -> Result<Option<(String, ApiKeySource)>> {
    if let Some(var) = env_var {
        if let Ok(value) = std::env::var(var) {
            if !value.is_empty() {
                let use_env = Confirm::new(&format!(
                    "{var} is set in the environment. Use it? (the value is not printed)"
                ))
                .with_default(true)
                .prompt()?;
                if use_env {
                    return Ok(Some((value, ApiKeySource::Env(var.to_string()))));
                }
            }
        }
    }
    let mut prompt = Text::new("  API key (leave empty to configure later)");
    if let Some(var) = env_var {
        prompt = prompt.with_placeholder(var);
    }
    let key = prompt.prompt()?.trim().to_string();
    Ok(if key.is_empty() {
        None
    } else {
        Some((key, ApiKeySource::Manual))
    })
}

/// Offline fallback: the pre-catalog DeepSeek flow, unchanged.
fn prompt_llm_fallback() -> Result<LlmPromptOutcome> {
    let api_key = std::env::var("DEEPSEEK_API_KEY").unwrap_or_default();
    let api_base = std::env::var("DEEPSEEK_BASE_URL").unwrap_or_default();
    let has_key = !api_key.is_empty();

    if has_key {
        let write_key = Confirm::new(
            "DEEPSEEK_API_KEY detected. Write it to the config file?\n  \
             If no, pass it via LLM_CONFIG env or --llm-config at runtime",
        )
        .with_default(false)
        .prompt()?;
        if write_key {
            let llm_config = format!(
                "[[llm]]\nprovider = \"openai\"\nmodel = \"deepseek-chat\"\
                 \napi_key = \"{api_key}\"\napi_base = \"{api_base}/v1\"\
                 \nmax_tokens = 4096\ntemperature = 0.3\n\n"
            );
            Ok(LlmPromptOutcome::new(
                llm_config,
                "DEEPSEEK_API_KEY configured".to_string(),
            ))
        } else {
            Ok(LlmPromptOutcome::new(String::new(), "via LLM_CONFIG env".to_string()))
        }
    } else {
        let llm_config = "\
# Fill in your LLM credentials, or pass them via env:\n\
# [[llm]]\n\
# provider = \"openai\"\n\
# model = \"deepseek-chat\"\n\
# api_key = \"sk-...\"\n\
# api_base = \"https://api.deepseek.com\"\n\
max_tokens = 4096\n\
temperature = 0.3\n\n"
            .to_string();
        Ok(LlmPromptOutcome::new(
            llm_config,
            "no API key found, configure manually".to_string(),
        ))
    }
}

/// Prompt the user to select experts for the review team.
fn prompt_experts() -> Result<Vec<&'static (&'static str, u8, &'static str, &'static str)>> {
    section("Expert Team");
    let expert_displays: Vec<String> = AVAILABLE_EXPERTS.iter().map(fmt_expert).collect();
    let expert_selected = MultiSelect::new("Select experts", expert_displays.clone())
        .with_default(&[])
        .with_formatter(&|_| String::new())
        .prompt()?;

    if expert_selected.is_empty() {
        anyhow::bail!("At least one expert must be selected");
    }
    let expert_indices: Vec<usize> = expert_selected
        .iter()
        .filter_map(|s| expert_displays.iter().position(|d| d == s))
        .collect();
    println!();
    for &i in &expert_indices {
        let (name, weight, _role, desc) = AVAILABLE_EXPERTS[i];
        println!("    \u{2022} {name} (weight {weight}) \u{2014} {desc}");
    }
    println!();

    Ok(expert_indices.iter().map(|&i| &AVAILABLE_EXPERTS[i]).collect())
}

/// Prompt the user for weight allocation and compute final weights.
fn compute_weights(selected: &[&(&str, u8, &str, &str)]) -> Result<Vec<u8>> {
    let weight_items = vec!["  Auto (scale defaults to 100)", "  Manual"];
    let weight_auto = Select::new("Weight allocation method", weight_items)
        .with_starting_cursor(0)
        .prompt()?;
    let is_auto = weight_auto == "  Auto (scale defaults to 100)";

    if is_auto {
        let total_default: u32 = selected.iter().map(|(_, w, _, _)| *w as u32).sum();
        let mut weights: Vec<u8> = selected
            .iter()
            .map(|(_, w, _, _)| ((*w as f64 / total_default as f64) * 100.0).round() as u8)
            .collect();

        // Ensure the sum of weights is exactly 100 after rounding
        let sum: u32 = weights.iter().map(|&w| w as u32).sum();
        if sum != 100 {
            let diff = (100i32 - sum as i32) as i8;
            if let Some(max_idx) = weights.iter().enumerate().max_by_key(|(_, w)| *w).map(|(i, _)| i) {
                let adjusted = (weights[max_idx] as i16 + diff as i16).clamp(0, 255) as u8;
                weights[max_idx] = adjusted;
            }
        }
        Ok(weights)
    } else {
        let mut ws = Vec::new();
        for (name, _def_w, _role, _desc) in selected {
            let w: String = Text::new(&format!("  Weight for {name}")).with_default("10").prompt()?;
            ws.push(w.parse::<u8>().unwrap_or(10));
        }
        Ok(ws)
    }
}

/// Prompt for review parameters (max findings, large PR threshold).
fn prompt_review_params() -> Result<(u32, u32)> {
    section("Review Parameters");
    let max_findings: u32 = Text::new("  Max findings per expert")
        .with_default("5")
        .prompt()?
        .parse()
        .unwrap_or(5);

    let large_pr_threshold: u32 = Text::new("  Large PR file threshold")
        .with_default("21")
        .prompt()?
        .parse()
        .unwrap_or(21);

    Ok((max_findings, large_pr_threshold))
}

/// Print a summary of the selected configuration.
fn print_summary(
    dominant: &str,
    cmd_indices: &[usize],
    llm_note: &str,
    selected: &[&(&str, u8, &str, &str)],
    max_findings: u32,
    large_pr_threshold: u32,
) {
    let cmd_names: Vec<String> = AVAILABLE_COMMANDS
        .iter()
        .enumerate()
        .filter(|(i, _)| cmd_indices.contains(i))
        .map(|(_, (name, _))| name.to_string())
        .collect();

    let expert_names: Vec<String> = selected.iter().map(|(n, _, _, _)| n.to_string()).collect();

    println!();
    println!("  {}", "─".repeat(40));
    println!("  Configuration summary");
    println!("  {}", "─".repeat(40));
    println!("  Language:     {dominant}");
    println!("  Commands:     {}", cmd_names.join(", "));
    println!("  LLM:          {llm_note}");
    println!("  Experts:      {}", expert_names.join(", "));
    println!("  Max findings: {max_findings}");
    println!("  Large PR:     {large_pr_threshold} files");
    println!("  {}", "─".repeat(40));
    println!();
}

/// Generate the TOML configuration string from chosen settings.
fn generate_toml(
    dominant: &str,
    cmd_indices: &[usize],
    llm_config: &str,
    selected: &[&(&str, u8, &str, &str)],
    weights: &[u8],
    max_findings: u32,
    large_pr_threshold: u32,
) -> String {
    let mut toml = String::new();
    toml.push_str("# Auto-generated by `review-engine init`\n");
    toml.push_str("# Review your codebase with:\n");
    toml.push_str("#   review-engine repo-review --local-path .\n\n");

    toml.push_str("[project]\n");
    toml.push_str("name = \"default\"\n\n");

    toml.push_str("[report]\n");
    toml.push_str(&format!("max_findings_per_expert = {max_findings}\n"));
    toml.push_str("aggregated = false\n\n");

    toml.push_str("[commands]\n");
    for (i, (name, _)) in AVAILABLE_COMMANDS.iter().enumerate() {
        let enabled = cmd_indices.contains(&i);
        let snake_name = name.replace('-', "_");
        toml.push_str(&format!("{} = {}\n", snake_name, enabled));
    }
    toml.push('\n');

    toml.push_str("[languages]\n");
    toml.push_str(&format!("dominant = \"{dominant}\"\n\n"));

    if !llm_config.is_empty() {
        toml.push_str(llm_config);
    }

    toml.push_str("[scoring]\n");
    toml.push_str("enabled = true\n");
    toml.push_str("display_individual_scores = true\n");
    toml.push_str("display_weighted_score = true\n\n");

    toml.push_str("[diff]\n");
    toml.push_str("max_input_tokens = 120000\n");
    toml.push_str(&format!("large_pr_file_threshold = {large_pr_threshold}\n"));
    toml.push_str("compression_level = \"auto\"\n\n");

    for (idx, (name, _def_w, role, _desc)) in selected.iter().enumerate() {
        let weight = weights[idx];
        let expert_cmds: Vec<String> = cmd_indices
            .iter()
            .map(|&i| AVAILABLE_COMMANDS[i].0.replace('-', "_"))
            .collect();
        // Empty command list must render as `commands = []` (an empty array),
        // not `commands = [[]]` (an array containing an empty array) — the
        // latter fails to deserialize as `Vec<String>`.
        let cmds_str = if expert_cmds.is_empty() {
            "[]".to_string()
        } else {
            let inner = expert_cmds
                .iter()
                .map(|c| format!("\"{}\"", c))
                .collect::<Vec<_>>()
                .join(", ");
            format!("[{inner}]")
        };
        toml.push_str(&format!("[review_experts.{name}]\n"));
        toml.push_str("enabled = true\n");
        toml.push_str(&format!("role = \"{role}\"\n"));
        toml.push_str(&format!("weight = {weight}\n"));
        toml.push_str(&format!("commands = {cmds_str}\n\n"));
    }

    toml
}

/// Run the interactive init flow.
pub async fn run_interactive(local_path: &str) -> Result<()> {
    // Scan project
    let scan = scan_project(local_path)?;
    print_header(
        &scan.dominant,
        scan.has_ci,
        scan.has_test,
        scan.total_files,
        scan.total_loc,
    );

    // Prompt for configuration
    let cmd_indices = prompt_commands()?;
    let llm = prompt_llm().await?;
    let selected = prompt_experts()?;
    let weights = compute_weights(&selected)?;
    let (max_findings, large_pr_threshold) = prompt_review_params()?;

    // Print summary
    print_summary(
        &scan.dominant,
        &cmd_indices,
        &llm.note,
        &selected,
        max_findings,
        large_pr_threshold,
    );

    // Ask for save path
    let path: String = Text::new("  Save to")
        .with_default(".code-audit-config.toml")
        .prompt()?;

    // Generate and write TOML
    let toml = generate_toml(
        &scan.dominant,
        &cmd_indices,
        &llm.block,
        &selected,
        &weights,
        max_findings,
        large_pr_threshold,
    );

    std::fs::write(&path, &toml)?;
    println!("  \u{2713} 已生成 {path}");
    for hint in &llm.post_init_hints {
        println!("  \u{2139} {hint}");
    }

    Ok(())
}

/// Write the built-in default configuration to `.code-audit-config.toml`.
pub fn run_default() -> Result<()> {
    let config = default_config()?;
    let toml = toml::to_string_pretty(&config)?;
    std::fs::write(".code-audit-config.toml", toml)?;
    println!("Created .code-audit-config.toml");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Select `count` experts from the front of `AVAILABLE_EXPERTS`.
    fn take_experts(count: usize) -> Vec<&'static (&'static str, u8, &'static str, &'static str)> {
        AVAILABLE_EXPERTS.iter().take(count).collect()
    }

    #[test]
    fn available_commands_are_unique_and_non_empty() {
        assert_eq!(AVAILABLE_COMMANDS.len(), 6);
        let mut names: Vec<&str> = AVAILABLE_COMMANDS.iter().map(|(n, _)| *n).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), AVAILABLE_COMMANDS.len(), "command names must be unique");
        assert!(AVAILABLE_COMMANDS.iter().all(|(n, d)| !n.is_empty() && !d.is_empty()));
    }

    #[test]
    fn available_experts_have_unique_names_and_positive_weights() {
        let mut names: Vec<&str> = AVAILABLE_EXPERTS.iter().map(|(n, _, _, _)| *n).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), AVAILABLE_EXPERTS.len(), "expert names must be unique");
        assert!(AVAILABLE_EXPERTS
            .iter()
            .all(|(n, w, r, d)| !n.is_empty() && *w > 0 && !r.is_empty() && !d.is_empty()));
    }

    #[test]
    fn fmt_cmd_pads_name_and_keeps_description() {
        let line = fmt_cmd(&("review", "MR/PR code review"));
        assert!(line.starts_with("  "));
        // 15-char left-aligned name field, then the description.
        let expected = format!("  {:<15}  {}", "review", "MR/PR code review");
        assert_eq!(line, expected);
    }

    #[test]
    fn fmt_expert_includes_name_weight_and_description() {
        let line = fmt_expert(&("security", 15, "Security Lead", "vulnerability & threat analysis"));
        assert!(line.starts_with("  security"));
        assert!(line.contains("weight 15"));
        assert!(line.contains("vulnerability & threat analysis"));
    }

    #[test]
    fn generate_toml_emits_project_and_command_toggles() {
        let selected = take_experts(2);
        let weights = vec![60, 40];
        let toml = generate_toml("Rust", &[0, 1], "", &selected, &weights, 5, 21);

        assert!(toml.contains("# Auto-generated by `review-engine init`"));
        assert!(toml.contains("[project]"));
        assert!(toml.contains("name = \"default\""));
        assert!(toml.contains("dominant = \"Rust\""));
        // Command toggles: review + repo_review enabled, the rest disabled.
        assert!(toml.contains("review = true"));
        assert!(toml.contains("repo_review = true"));
        assert!(toml.contains("improve = false"));
        assert!(toml.contains("ask = false"));
        assert!(toml.contains("max_findings_per_expert = 5"));
        assert!(toml.contains("large_pr_file_threshold = 21"));
        // Both selected experts are emitted with their weights.
        assert!(toml.contains("[review_experts.lead]"));
        assert!(toml.contains("weight = 60"));
        assert!(toml.contains("[review_experts.security]"));
        assert!(toml.contains("weight = 40"));
        // Commands are shared into every expert's command list.
        assert!(toml.contains("commands = [\"review\", \"repo_review\"]"));
    }

    #[test]
    fn generate_toml_with_no_commands_emits_empty_command_list() {
        let selected = take_experts(1);
        let weights = vec![100];
        let toml = generate_toml("Python", &[], "", &selected, &weights, 3, 10);

        assert!(toml.contains("review = false"));
        assert!(toml.contains("repo_review = false"));
        assert!(toml.contains("commands = []"));
        assert!(toml.contains("dominant = \"Python\""));
        assert!(toml.contains("max_findings_per_expert = 3"));
        assert!(toml.contains("large_pr_file_threshold = 10"));
    }

    #[test]
    fn generate_toml_appends_llm_config_when_non_empty() {
        let selected = take_experts(1);
        let weights = vec![100];
        let llm = "[[llm]]\nprovider = \"openai\"\nmodel = \"deepseek-chat\"\n";
        let toml = generate_toml("Go", &[0], llm, &selected, &weights, 5, 21);

        assert!(toml.contains(llm), "LLM block must be appended verbatim");
        assert!(toml.contains("[scoring]"));
        assert!(toml.contains("enabled = true"));
    }

    #[test]
    fn generate_toml_hyphenated_command_names_become_snake_case() {
        // `update_changelog` is already snake_case; a hypothetical hyphenated
        // name would be normalized. Only the real command set is exercised.
        let selected = take_experts(1);
        let weights = vec![100];
        let toml = generate_toml("Rust", &[5], "", &selected, &weights, 5, 21);
        assert!(toml.contains("update_changelog = true"));
    }

    #[test]
    fn generate_toml_weights_are_emitted_per_expert_in_order() {
        let selected = take_experts(3);
        let weights = vec![50, 30, 20];
        let toml = generate_toml("Java", &[0], "", &selected, &weights, 5, 21);
        assert!(toml.contains("weight = 50"));
        assert!(toml.contains("weight = 30"));
        assert!(toml.contains("weight = 20"));
        // Expert blocks appear in selection order.
        let lead = toml.find("[review_experts.lead]").expect("lead block");
        let security = toml.find("[review_experts.security]").expect("security block");
        let performance = toml.find("[review_experts.performance]").expect("performance block");
        assert!(
            lead < security && security < performance,
            "experts must be emitted in selection order"
        );
    }

    // ─── catalog-backed LLM prompt helpers ─────────────────────

    fn provider(id: &str, name: &str) -> CatalogProvider {
        CatalogProvider {
            id: id.to_string(),
            name: name.to_string(),
            api: Some(format!("https://api.{id}.example")),
            ..Default::default()
        }
    }

    #[test]
    fn curated_shortlist_orders_by_curated_list_not_catalog_order() {
        let catalog_providers = [
            provider("groq", "Groq"),
            provider("zzz", "ZZZ"),
            provider("openai", "OpenAI"),
            provider("deepseek", "DeepSeek"),
        ];
        let refs: Vec<&CatalogProvider> = catalog_providers.iter().collect();

        let shortlist = curated_shortlist(&refs);
        let ids: Vec<&str> = shortlist.iter().map(|p| p.id.as_str()).collect();
        // CURATED_PROVIDER_IDS order wins; catalog order and non-curated
        // entries are irrelevant.
        assert_eq!(ids, vec!["openai", "deepseek", "groq"]);
    }

    #[test]
    fn curated_shortlist_is_empty_when_no_curated_provider_present() {
        let catalog_providers = [provider("acme", "Acme")];
        let refs: Vec<&CatalogProvider> = catalog_providers.iter().collect();
        assert!(curated_shortlist(&refs).is_empty());
    }

    #[test]
    fn render_llm_block_with_key_emits_complete_block() {
        let block = render_llm_block(
            "deepseek",
            "deepseek-chat",
            "https://api.deepseek.com/v1",
            Some("sk-test"),
        );
        assert!(block.starts_with("[[llm]]\n"));
        assert!(block.contains("provider = \"deepseek\""));
        assert!(block.contains("model = \"deepseek-chat\""));
        assert!(block.contains("api_key = \"sk-test\""));
        assert!(block.contains("api_base = \"https://api.deepseek.com/v1\""));
        assert!(block.contains("max_tokens = 4096"));
        assert!(block.contains("temperature = 0.3"));
        assert!(!block.contains("# api_key"), "key line must not be commented out");
        // The block parses as TOML when embedded in a document.
        let parsed: toml::Value = toml::from_str(&block).expect("llm block must parse as TOML");
        assert_eq!(parsed["llm"][0]["provider"].as_str().unwrap(), "deepseek");
    }

    #[test]
    fn render_llm_block_without_key_comments_out_api_key_line() {
        let block = render_llm_block("openai", "gpt-4o", "https://api.openai.com/v1", None);
        assert!(block.contains("# api_key = \"...\""));
        assert!(!block.contains("\napi_key"), "no live api_key line");
        assert!(block.contains("provider = \"openai\""));
        let parsed: toml::Value = toml::from_str(&block).expect("keyless block must still parse as TOML");
        assert_eq!(parsed["llm"][0]["model"].as_str().unwrap(), "gpt-4o");
    }

    // ─── env-sourced API key handling ──────────────────────────

    #[test]
    fn plaintext_key_hint_names_env_var_and_runtime_channels() {
        let hint = plaintext_key_hint("DEEPSEEK_API_KEY");
        assert!(hint.contains("DEEPSEEK_API_KEY"), "hint must name the source env var");
        assert!(
            hint.contains("plaintext"),
            "hint must state the key was written in plaintext"
        );
        assert!(
            hint.contains("LLM_CONFIG"),
            "hint must point at the LLM_CONFIG env channel"
        );
        assert!(
            hint.contains("--llm-config"),
            "hint must point at the --llm-config flag"
        );
    }

    #[test]
    fn llm_note_distinguishes_key_sources() {
        let env_key = Some(("sk-live".to_string(), ApiKeySource::Env("DEEPSEEK_API_KEY".to_string())));
        assert_eq!(
            llm_note("DeepSeek", "deepseek-chat", &env_key),
            "DeepSeek / deepseek-chat (key from DEEPSEEK_API_KEY)"
        );

        let manual_key = Some(("sk-typed".to_string(), ApiKeySource::Manual));
        assert_eq!(
            llm_note("DeepSeek", "deepseek-chat", &manual_key),
            "DeepSeek / deepseek-chat"
        );

        assert_eq!(
            llm_note("DeepSeek", "deepseek-chat", &None),
            "DeepSeek / deepseek-chat (API key pending)"
        );
    }

    #[test]
    fn llm_prompt_outcome_defaults_to_no_hints() {
        let outcome = LlmPromptOutcome::new("[[llm]]\n".to_string(), "note".to_string());
        assert_eq!(outcome.block, "[[llm]]\n");
        assert_eq!(outcome.note, "note");
        assert!(outcome.post_init_hints.is_empty());
    }

    #[test]
    fn fmt_context_humanizes_limits() {
        assert_eq!(fmt_context(Some(128000)), "128k ctx");
        assert_eq!(fmt_context(Some(1000)), "1k ctx");
        assert_eq!(fmt_context(Some(64000)), "64k ctx");
        assert_eq!(fmt_context(Some(512)), "512 ctx");
        assert_eq!(fmt_context(None), "unknown context");
    }

    #[test]
    fn display_strings_carry_name_and_id() {
        let p = provider("deepseek", "DeepSeek");
        assert_eq!(provider_display(&p), "  DeepSeek (deepseek)");

        let m = CatalogModel {
            id: "deepseek-chat".to_string(),
            name: "DeepSeek Chat".to_string(),
            limit: Some(catalog::ModelLimit {
                context: Some(64000),
                output: Some(8192),
            }),
            ..Default::default()
        };
        assert_eq!(model_display(&m), "  DeepSeek Chat (deepseek-chat) — 64k ctx");

        let no_limit = CatalogModel {
            id: "m".to_string(),
            name: "M".to_string(),
            ..Default::default()
        };
        assert_eq!(model_display(&no_limit), "  M (m) — unknown context");
    }
}
