//! CLI command handlers, split by domain into submodules.
//!
//! This facade declares the submodules, re-exports the `pub` entry points so
//! `cli::mod` keeps calling `handlers::run_*` unchanged, and carries the
//! `pub(super)` helpers shared by the non-review command modules
//! (`single_expert_review_output` and `log_output_targets`, M34). The
//! output/LLM plumbing for the review flow still lives in `review` / `output`
//! (their `pub(super)` items feed the sibling command modules).

use review_engine::models::{ExpertReport, ReviewOutput};

pub mod ask;
pub mod changelog;
pub mod config;
pub mod describe;
pub mod improve;
pub mod output;
pub mod review;
pub mod upgrade;
pub mod upgrade_install;

pub use ask::{run_ask, run_ask_local_diff, run_ask_local_repo, run_ask_stdin};
pub use changelog::run_update_changelog;
pub use config::provider::run as run_config_provider;
pub use config::watch_config_file;
pub use describe::{run_describe, run_describe_local_diff, run_describe_local_repo};
pub use improve::{run_improve, run_improve_local_diff, run_improve_local_repo};
pub use review::{
    resolve_llm_configs, run_local, run_local_path, run_local_repo, run_mr, run_repo_review_local_or_enhanced,
    run_stdin,
};
pub use upgrade::run_upgrade;

/// Build a `ReviewOutput` wrapping a single non-review expert report.
///
/// `ask`, `describe` and `improve` are not expert-team reviews: each produces
/// exactly one report with no findings, no aggregator, no lead consolidation,
/// and no verification pass. The 14-line `ReviewOutput { ... }` literal used
/// to appear three times in each of the three handlers — this constructor is
/// the one place that decides the shape, so a future field lands in every
/// site at once. The `expert_name` carries the command name (`"ask"`,
/// `"describe"`, `"improve"`); `markdown` is the rendered response text.
pub(super) fn single_expert_review_output(expert_name: &str, markdown: String) -> ReviewOutput {
    ReviewOutput {
        reports: vec![ExpertReport {
            expert_name: expert_name.to_string(),
            findings: vec![],
            markdown,
            raw_llm_response: String::new(),
            parse_error: None,
            raw_dump_path: None,
            llm_model: None,
            llm_fp: None,
            llm_provider: None,
        }],
        aggregated: None,
        dropped_findings: vec![],
        consolidated: None,
        errors: vec![],
    }
}

/// Log the two output sinks the CLI dispatch layer is about to use.
///
/// W13 priority, highest first:
/// 1. `--output <file>` (when set) is the absolute primary write target —
///    the user named it, so it wins over anything implicit.
/// 2. `config.output_dir` (when non-empty) still receives a timestamped copy
///    of the same content via [`super::output::write_report_text`].
///
/// Both targets are written, and both are logged here so the priority is
/// visible in `logs.ndjson` and the Web UI Logs page — a silent dual-write
/// surprises users who expect the explicit `--output` to be the only file
/// on disk. Called by every `ask` / `describe` / `improve` handler right
/// before `write_output`; the dispatch layer in `app.rs` emits the same
/// trace at the Review arms so the priority reads identically there.
pub(super) fn log_output_targets(command: &str, output: &Option<String>, output_dir: &str) {
    if let Some(path) = output {
        tracing::info!(
            command = command,
            "--output = {} (explicit file overrides any default path)",
            path
        );
    }
    if !output_dir.is_empty() {
        tracing::info!(command = command, "timestamped copy under output_dir = {}", output_dir);
    }
}
