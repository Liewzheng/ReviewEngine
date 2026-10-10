use anyhow::{Context, Result};
use review_engine::models::*;
use std::path::{Path, PathBuf};

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
                .map(|r| review_engine::output::team_renderer::render_expert_section(r))
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

/// Persist `text` to `path` with `0600` permissions on Unix.
///
/// RENG-113: the report file is the canonical local copy of the review — by
/// default it lives at `~/.config/review-engine/reports/review_*.md` with
/// the user's umask. A `0644` mode lets every other account on the same host
/// read it, which means a quoted `password = "real_password"` line in
/// Evidence (already redacted at the renderer, but still — defence in
/// depth) becomes readable. `0600` keeps the file owner-only.
///
/// RENG-113 round 2: the round-1 implementation wrote first and chmod'd
/// after — the file briefly existed with umask-derived perms (typically
/// `0644`) and an interrupted run left it that way. Round 2 creates the
/// file with `OpenOptions::mode(0o600)` so the file is born with the
/// right perms and never sits in a permissive window. The chmod-after step
/// stays for the overwrite path (truncate doesn't change mode on existing
/// files; the chmod covers pre-existing files that already have a
/// permissive mode).
///
/// On non-Unix targets the function falls through to a plain `fs::write`;
/// Windows file ACLs are not modelled here.
fn write_report_file(path: &Path, text: &str) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        // `create(true).write(true).truncate(true).mode(0o600)` is the
        // single syscall path: the inode's mode is set before the file
        // becomes reachable on the filesystem, so there is no window
        // where another process can `open()` it under a permissive mode.
        // `mode()` is an OpenOptionsExt method (Unix-only).
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("failed to create {}", path.display()))?;
        std::io::Write::write_all(&mut file, text.as_bytes())
            .with_context(|| format!("failed to write {}", path.display()))?;
        // Belt-and-braces: if the file already existed (truncate path),
        // the kernel may have left its old mode alone. Force `0600` again.
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to chmod 0600 {}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, text)?;
    }
    Ok(())
}

/// Persist `text` as a timestamped report under `output_dir` (the default
/// reports directory) and return the file path. Shared by both `--output`
/// branches so every run — explicit file or stdout — leaves a copy in the
/// reports directory.
fn save_timestamped_report(text: &str, format: &str, output_dir: &str) -> Result<PathBuf> {
    let dir = std::path::Path::new(output_dir);
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
    write_report_file(&filepath, text)?;
    Ok(filepath)
}

/// Unified sink for rendered report text.
///
/// * `--output <file>`: write the explicit file (path validated against `..`
///   traversal) AND the same content to a timestamped file under
///   `output_dir`.
/// * no `--output`: print to stdout, plus the same timestamped copy.
///
/// The timestamped copy is skipped only when `output_dir` is `None`.
///
/// Both the `--output` file and the timestamped copy are written with
/// `0600` on Unix (RENG-113): the report carries evidence text and other
/// LLM-derived prose that may quote a config line, and `0644` would let any
/// other account on the host read it.
pub(super) fn write_report_text(
    text: &str,
    format: &str,
    output: &Option<String>,
    output_dir: Option<&str>,
) -> Result<()> {
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
            write_report_file(path, text)?;
        }
        None => {
            // No explicit output: print to stdout
            println!("{}", text);
        }
    }
    if let Some(dir) = output_dir {
        let filepath = save_timestamped_report(text, format, dir)?;
        eprintln!("Report saved to {}", filepath.display());
    }
    Ok(())
}

pub(super) fn write_output(
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

    write_report_text(&text, format, output, output_dir)
}

#[cfg(test)]
mod tests {
    use super::super::review::prepare_review;
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
            confidence_downgraded: 0,
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
                unverified: false,
                coverage_insufficient: false,
            },
            consensus_reached: true,
            total_files: 0,
            reviewed_files: 0,
            unreviewed_files: vec![],
            coverage: None,
            parse_status: vec![],
            verification_stats: None,
            injected_context: Default::default(),
            adjudicated_removed: vec![],
        }
    }

    fn sample_output(consolidated: Option<ConsolidatedReport>) -> ReviewOutput {
        ReviewOutput {
            reports: vec![ExpertReport {
                expert_name: "security".to_string(),
                findings: vec![make_finding(Severity::High, "src/main.rs")],
                markdown: "## Security Review\n\nSome findings.\n".to_string(),
                raw_llm_response: String::new(),
                parse_error: None,
                raw_dump_path: None,
                llm_model: None,
                llm_fp: None,
                llm_provider: None,
            }],
            aggregated: None,
            dropped_findings: vec![],
            consolidated,
            errors: vec![],
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

#[cfg(test)]
mod report_output_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::super::review::run_repo_review_local_or_enhanced;
    use super::*;
    use tempfile::tempdir;

    fn test_config(output_dir: &str) -> AppConfig {
        AppConfig {
            project: None,
            report: Default::default(),
            review_experts: Default::default(),
            commands: Default::default(),
            scoring: Default::default(),
            llm: vec![],
            max_team_size: None,
            max_concurrent_llm_calls: None,
            output_dir: output_dir.to_string(),
            diff: Default::default(),
            rate_limit: Default::default(),
            languages: Default::default(),
        }
    }

    fn saved_files(dir: &Path) -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.path()).collect();
        files.sort();
        files
    }

    #[test]
    fn write_report_text_output_also_saves_timestamped_copy() {
        let dir = tempdir().unwrap();
        let reports = dir.path().join("reports");
        let reports_str = reports.to_string_lossy().to_string();
        let explicit = dir.path().join("custom.md");
        write_report_text(
            "# hello",
            "markdown",
            &Some(explicit.to_string_lossy().to_string()),
            Some(&reports_str),
        )
        .unwrap();

        assert_eq!(std::fs::read_to_string(&explicit).unwrap(), "# hello");
        let saved = saved_files(&reports);
        assert_eq!(saved.len(), 1, "exactly one timestamped copy: {saved:?}");
        let name = saved[0].file_name().unwrap().to_string_lossy().to_string();
        assert!(name.starts_with("review_") && name.ends_with(".md"), "{name}");
        assert_eq!(
            std::fs::read_to_string(&saved[0]).unwrap(),
            "# hello",
            "timestamped copy holds the same content as the --output file"
        );
    }

    #[test]
    fn write_report_text_default_run_saves_timestamped_copy() {
        let dir = tempdir().unwrap();
        let reports = dir.path().join("reports");
        let reports_str = reports.to_string_lossy().to_string();
        write_report_text("{\"ok\":true}", "json", &None, Some(&reports_str)).unwrap();
        let saved = saved_files(&reports);
        assert_eq!(saved.len(), 1, "{saved:?}");
        let name = saved[0].file_name().unwrap().to_string_lossy().to_string();
        assert!(name.starts_with("review_") && name.ends_with(".json"), "{name}");
        assert_eq!(std::fs::read_to_string(&saved[0]).unwrap(), "{\"ok\":true}");
    }

    #[test]
    fn write_report_text_without_output_dir_writes_only_explicit_file() {
        let dir = tempdir().unwrap();
        let explicit = dir.path().join("only.md");
        write_report_text("x", "markdown", &Some(explicit.to_string_lossy().to_string()), None).unwrap();
        assert_eq!(saved_files(dir.path()), vec![explicit]);
    }

    #[test]
    fn write_report_text_rejects_parent_dir_traversal() {
        let dir = tempdir().unwrap();
        let reports_str = dir.path().join("reports").to_string_lossy().to_string();
        let err = write_report_text("x", "markdown", &Some("../evil.md".to_string()), Some(&reports_str)).unwrap_err();
        assert!(err.to_string().contains("must not contain '..'"), "{err}");
        let err = write_report_text("x", "markdown", &None, Some("../evil-reports")).unwrap_err();
        assert!(err.to_string().contains("must not contain '..'"), "{err}");
    }

    #[tokio::test]
    async fn repo_review_default_run_lands_on_disk() {
        let repo = tempdir().unwrap();
        std::fs::write(repo.path().join("main.rs"), "fn main() {}\n").unwrap();
        let reports = tempdir().unwrap();
        let config = test_config(&reports.path().to_string_lossy());

        run_repo_review_local_or_enhanced(
            repo.path().to_str().unwrap(),
            &[],
            "json",
            &None,
            None,
            "test-rr-default",
            &config,
        )
        .await
        .unwrap();

        let saved = saved_files(reports.path());
        assert_eq!(
            saved.len(),
            1,
            "default run must write one timestamped report: {saved:?}"
        );
        let name = saved[0].file_name().unwrap().to_string_lossy().to_string();
        assert!(name.starts_with("review_") && name.ends_with(".json"), "{name}");
        let value: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&saved[0]).unwrap()).unwrap();
        assert!(value["overview"]["health_score"].is_number());
        assert!(
            value["metadata"]["tree_hash"].is_string(),
            "provenance metadata must be in the saved report"
        );
    }

    #[tokio::test]
    async fn repo_review_output_double_writes_timestamped_copy() {
        let repo = tempdir().unwrap();
        std::fs::write(repo.path().join("main.rs"), "fn main() {}\n").unwrap();
        let out_root = tempdir().unwrap();
        let explicit = out_root.path().join("report.json");
        let reports = out_root.path().join("reports");
        let config = test_config(&reports.to_string_lossy());
        let output = Some(explicit.to_string_lossy().to_string());

        run_repo_review_local_or_enhanced(
            repo.path().to_str().unwrap(),
            &[],
            "json",
            &output,
            None,
            "test-rr-double",
            &config,
        )
        .await
        .unwrap();

        assert!(explicit.exists(), "--output file must be written");
        let saved = saved_files(&reports);
        assert_eq!(saved.len(), 1, "--output must also drop a timestamped copy: {saved:?}");
        assert_eq!(
            std::fs::read_to_string(&saved[0]).unwrap(),
            std::fs::read_to_string(&explicit).unwrap(),
            "timestamped copy and --output file hold identical content"
        );
    }

    // ── RENG-113: report files must land with 0600 on Unix ──────────────

    /// The timestamped copy under `output_dir` is the canonical local copy
    /// of the review. RENG-113: by default it lands at
    /// `~/.config/review-engine/reports/review_*.md` with the user's umask,
    /// which on most distros is `0644` and lets every other account on the
    /// host read it. The report carries Evidence blocks and other LLM prose
    /// — even with the redaction applied at the renderer, `0600` is the
    /// "owner only" baseline.
    #[cfg(unix)]
    #[test]
    fn write_report_text_lands_the_timestamped_copy_at_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir().unwrap();
        let reports = dir.path().join("reports");
        let reports_str = reports.to_string_lossy().to_string();
        write_report_text("secret", "markdown", &None, Some(&reports_str)).unwrap();
        let saved = saved_files(&reports);
        assert_eq!(saved.len(), 1);
        let mode = std::fs::metadata(&saved[0]).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "the timestamped report file must be owner-only, got {:o}",
            mode
        );
    }

    /// Same baseline for the explicit `--output` file: even when the user
    /// pointed the report at a custom path, the file mode is `0600`.
    #[cfg(unix)]
    #[test]
    fn write_report_text_lands_the_explicit_output_file_at_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir().unwrap();
        let explicit = dir.path().join("explicit.md");
        write_report_text(
            "secret",
            "markdown",
            &Some(explicit.to_string_lossy().to_string()),
            None,
        )
        .unwrap();
        let mode = std::fs::metadata(&explicit).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the --output file must be owner-only, got {:o}", mode);
    }

    // RENG-113 round 2: the round-1 `fs::write` then chmod path left a
    // transient window where the file was reachable at the umask's
    // default (typically `0644`). Round 2 creates the file with
    // `OpenOptions::mode(0o600)` and the file is born at `0600`. The
    // overwrite path must still land at `0600` too — the truncate path
    // inherits the existing file's mode if no chmod runs, which is the
    // round-1 bug we are closing.
    #[cfg(unix)]
    #[test]
    fn write_report_text_overwrites_an_existing_file_at_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir().unwrap();
        let explicit = dir.path().join("overwrite.md");
        // First write — lands at 0600.
        write_report_text(
            "first body",
            "markdown",
            &Some(explicit.to_string_lossy().to_string()),
            None,
        )
        .unwrap();
        // Second write — must overwrite and stay at 0600.
        write_report_text(
            "second body",
            "markdown",
            &Some(explicit.to_string_lossy().to_string()),
            None,
        )
        .unwrap();

        let mode = std::fs::metadata(&explicit).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the overwrite path must keep 0600, got {:o}", mode);
        assert_eq!(std::fs::read_to_string(&explicit).unwrap(), "second body");
    }

    /// RENG-113 round 2: a pre-existing file with permissive mode must be
    /// brought back to `0600` by the next write. The OpenOptions path is
    /// primary for new files; the chmod after-step tightens files already on
    /// disk — together they close the round-1 transient window.
    #[cfg(unix)]
    #[test]
    fn write_report_text_tightens_a_pre_existing_permissive_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir().unwrap();
        let explicit = dir.path().join("preexisting.md");
        // Drop a file with `0644` mode — the round-1 transient window
        // value, the very thing the OpenOptions fix closes.
        std::fs::write(&explicit, "old").unwrap();
        std::fs::set_permissions(&explicit, std::fs::Permissions::from_mode(0o644)).unwrap();

        write_report_text(
            "new body",
            "markdown",
            &Some(explicit.to_string_lossy().to_string()),
            None,
        )
        .unwrap();

        let mode = std::fs::metadata(&explicit).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "the write must tighten a permissive file to 0600, got {:o}",
            mode
        );
        assert_eq!(std::fs::read_to_string(&explicit).unwrap(), "new body");
    }
}
