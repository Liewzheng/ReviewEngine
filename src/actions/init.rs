//! `review-engine init` — interactive project initialization.
//!
//! Scans the repository, detects languages / CI / test frameworks, prompts
//! the user for preferences (commands, experts, LLM), and writes the
//! resulting `.code-audit-config.toml` to disk.

use anyhow::Result;
use inquire::{Confirm, MultiSelect, Select, Text};

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
    ("repo-review", "full repo health check"),
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

/// Prompt the user for LLM configuration.
fn prompt_llm() -> Result<(String, String)> {
    section("LLM (AI Review)");
    let enable_llm = Confirm::new("Enable LLM-based AI review? (skip for local-only static analysis)")
        .with_default(true)
        .prompt()?;

    if !enable_llm {
        return Ok((String::new(), "disabled".to_string()));
    }

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
            Ok((llm_config, "DEEPSEEK_API_KEY configured".to_string()))
        } else {
            Ok((String::new(), "via LLM_CONFIG env".to_string()))
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
        Ok((llm_config, "no API key found, configure manually".to_string()))
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
        Ok(selected
            .iter()
            .map(|(_, w, _, _)| ((*w as f64 / total_default as f64) * 100.0).round() as u8)
            .collect())
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
        toml.push_str(&format!("{} = {}\n", name, enabled));
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
    toml.push_str("compression_level = \"aggressive\"\n\n");

    for (idx, (name, _def_w, role, _desc)) in selected.iter().enumerate() {
        let weight = weights[idx];
        toml.push_str(&format!("[review_experts.{name}]\n"));
        toml.push_str("enabled = true\n");
        toml.push_str(&format!("role = \"{role}\"\n"));
        toml.push_str(&format!("weight = {weight}\n"));
        toml.push_str("commands = [\"review\", \"repo-review\"]\n\n");
    }

    toml
}

/// Run the interactive init flow.
pub fn run_interactive(local_path: &str) -> Result<()> {
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
    let (llm_config, llm_note) = prompt_llm()?;
    let selected = prompt_experts()?;
    let weights = compute_weights(&selected)?;
    let (max_findings, large_pr_threshold) = prompt_review_params()?;

    // Print summary
    print_summary(
        &scan.dominant,
        &cmd_indices,
        &llm_note,
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
        &llm_config,
        &selected,
        &weights,
        max_findings,
        large_pr_threshold,
    );

    std::fs::write(&path, &toml)?;
    println!("  \u{2713} 已生成 {path}");

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
