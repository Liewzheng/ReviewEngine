use anyhow::Result;
use async_trait::async_trait;

use super::super::{ExpertScore, RepoContext, RepoExpert, ScoreItem};
use crate::llm::client::LLMClient;

// ─── Dependency ───────────────────────────────

/// Static expert that evaluates dependency health across ecosystems.
///
/// Recognises the lockfile / module-manifest that each major ecosystem
/// uses to declare its dependencies — `Cargo.lock` (Rust), `go.mod`
/// (Go, both single-line and block-form `require` syntax), the
/// `dependencies` and `packages` maps inside `package-lock.json`
/// (npm / pnpm), `poetry.lock`'s `[[package]]` blocks (Poetry / Python),
/// and the `pins` array of `Package.resolved` (SwiftPM) — counts them
/// per ecosystem, and flags repositories with more than 200 total
/// dependencies for audit.
///
/// The summary names each detected ecosystem alongside its count, so
/// a Go repository never sees a "0 dependencies from Cargo.lock" line
/// just because the expert was scanning for the wrong file. When no
/// recognised lockfile is present the expert emits an explicit
/// neutral summary together with a `[hygiene]`-tagged reminder
/// ("no dependency lockfile detected — dependency auditing skipped")
/// instead of a false perfect-score claim. The >200 audit's
/// recommendation is also ecosystem-specific — `cargo audit` for
/// Cargo, `govulncheck` for Go, `npm audit` for npm, etc. Does not
/// require an LLM.
pub struct Dependency;

/// Per-ecosystem snapshot collected from one or more recognised
/// lockfiles. The `ecosystem` is the human label printed in the
/// summary; `counts` is the list of dependency counts extracted from
/// each lockfile of that ecosystem (one entry per lockfile). A
/// monorepo that ships `frontend/package-lock.json` +
/// `backend/package-lock.json` therefore has `counts = vec![12, 8]`
/// rather than two separate `EcosystemCount`s with the same label —
/// the latter would render as `"npm (12) + npm (8)"` and the reader
/// would have to dedupe by eye. `counts` is always non-empty
/// (we never construct an `EcosystemCount` with no detected
/// lockfiles). The path the files came from is intentionally not
/// retained — the summary only needs the ecosystem name and the
/// count(s), and the >200 audit recommendation names the auditing
/// tool per ecosystem rather than the lockfile path.
struct EcosystemCount {
    ecosystem: &'static str,
    counts: Vec<usize>,
}

impl EcosystemCount {
    /// Total dependency count across every per-lockfile count in this
    /// ecosystem.
    fn total(&self) -> usize {
        self.counts.iter().sum()
    }

    /// Render the per-ecosystem count list as it appears inside the
    /// summary's parens — `"12"` for a single lockfile,
    /// `"12 + 8"` for two, `"12 + 8 + 3"` for three, etc.
    fn render_counts(&self) -> String {
        let parts: Vec<String> = self.counts.iter().map(|c| c.to_string()).collect();
        parts.join(" + ")
    }
}

/// Coalesce per-ecosystem counts so a monorepo with multiple lockfiles
/// of the same ecosystem collapses into a single labelled entry.
/// Insertion order is the order the lockfiles were encountered in the
/// scan; we preserve it so the reader sees the same order the scanner
/// did.
fn coalesce_ecosystem_counts(counts: Vec<EcosystemCount>) -> Vec<EcosystemCount> {
    use std::collections::BTreeMap;
    // Order-preserving merge: we walk the input in order, appending
    // each per-lockfile count to the slot keyed by ecosystem. A
    // `BTreeMap` happens to keep the keys sorted, which is the same
    // behaviour the per-ecosystem ordering had before this round
    // (the loop pushed ecosystem entries in scan order, and a
    // single-ecosystem repo therefore saw a single stable label
    // position; multi-ecosystem repos preserved scan order). When
    // the scan already produces ecosystem entries in alphabetical
    // order this is a no-op coalesce; when a monorepo produces two
    // `npm` entries it folds them into one.
    let mut merged: BTreeMap<&'static str, Vec<usize>> = BTreeMap::new();
    for entry in counts {
        merged.entry(entry.ecosystem).or_default().extend(entry.counts);
    }
    merged
        .into_iter()
        .map(|(ecosystem, counts)| EcosystemCount { ecosystem, counts })
        .collect()
}

/// Prefix used to mark findings that say "we could not assess this"
/// rather than "we found a real defect". Today the only such finding
/// is the no-lockfile reminder; the tag lets downstream surfaces
/// (and tests) distinguish "the expert had no lockfile to read"
/// from "the expert saw N deps and is flagging an audit", keeping the
/// reminder out of the quality-scoring channel.
const HYGIENE_TAG: &str = "[hygiene]";

/// Neutral score when no recognised lockfile exists. We can't assess
/// dependency health either way, so the score reflects "we don't
/// know" rather than the false-perfect 100 of the Cargo-only legacy
/// path or a punitive 0 that would drag the aggregate down on every
/// shell-only / docs-only / nascent repo.
///
/// This score is **paired with** `ExpertScore::fallback = true` so
/// the weighted-total math filters the placeholder out (RENG-115
/// contract: `experts::mod.rs:270-295`). Without `fallback = true`,
/// the placeholder would enter the headline aggregate and the report
/// would surface a "dependency 70/100 (medium)" row whose score is
/// genuinely meaningless. The score itself stays at 70 (the canonical
/// `LLM_FALLBACK_SCORE` band), the summary stays explicit, the
/// `[hygiene]`-tagged note stays visible — but the row leaves the
/// weighted total and the Top-Risks slot, exactly like the other
/// "could not assess" placeholders in the codebase.
const NO_LOCKFILE_SCORE: u8 = 70;

#[async_trait]
impl RepoExpert for Dependency {
    fn name(&self) -> &str {
        "dependency"
    }
    fn weight(&self) -> u8 {
        10
    }
    fn requires_llm(&self) -> bool {
        false
    }

    async fn evaluate(&self, ctx: &RepoContext, _llm: Option<&LLMClient>) -> Result<ExpertScore> {
        let mut details = Vec::new();
        let mut ecosystem_counts: Vec<EcosystemCount> = Vec::new();

        for entry in &ctx.entries {
            // Cheap pre-filter: check the basename before reading the
            // file. The expert only ever inspects five lockfile names,
            // so restricting I/O to those five candidates avoids
            // reading every scanned file (typically thousands of
            // entries on a large repo) just to discard 99% of them
            // on the basename match.
            let basename = std::path::Path::new(&entry.path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("");
            match basename {
                "Cargo.lock" | "go.mod" | "package-lock.json" | "poetry.lock" | "Package.resolved" => {}
                _ => continue,
            }
            let Ok(content) = std::fs::read_to_string(&entry.path) else {
                continue;
            };
            match basename {
                "Cargo.lock" => ecosystem_counts.push(EcosystemCount {
                    ecosystem: "Cargo",
                    counts: vec![count_cargo_lock(&content)],
                }),
                "go.mod" => ecosystem_counts.push(EcosystemCount {
                    ecosystem: "Go",
                    counts: vec![count_go_dependencies(&content)],
                }),
                "package-lock.json" => {
                    if let Some(count) = count_npm_dependencies(&content) {
                        ecosystem_counts.push(EcosystemCount {
                            ecosystem: "npm",
                            counts: vec![count],
                        });
                    }
                }
                "poetry.lock" => ecosystem_counts.push(EcosystemCount {
                    ecosystem: "Poetry",
                    counts: vec![count_poetry_dependencies(&content)],
                }),
                "Package.resolved" => ecosystem_counts.push(EcosystemCount {
                    ecosystem: "SwiftPM",
                    counts: vec![count_swiftpm_dependencies(&content)],
                }),
                _ => {}
            }
        }

        // Coalesce per-ecosystem counts so a monorepo that ships
        // several lockfiles of the same ecosystem (e.g. nested
        // `frontend/package-lock.json` + `backend/package-lock.json`)
        // reports one `"npm (12 + 8)"` label instead of two repeated
        // `"npm (12) + npm (8)"` labels. The order of detection is
        // preserved by the iteration above, so the secondary number
        // is the lockfile-scanned later in the repo walk.
        let ecosystem_counts = coalesce_ecosystem_counts(ecosystem_counts);

        // No recognised lockfile: explicit neutral summary + a single
        // hygiene reminder. The recommendation is ecosystem-aware so
        // a Go-shaped repo gets the `govulncheck` hint instead of the
        // `cargo audit` hint.
        if ecosystem_counts.is_empty() {
            details.push(ScoreItem {
                severity: "note".to_string(),
                message: format!("{HYGIENE_TAG} no dependency lockfile detected — dependency auditing skipped"),
                file: None,
                recommendation: Some(no_lockfile_audit_hint(&ctx.entries).to_string()),
                effort: Some("small".to_string()),
                ..Default::default()
            });
            return Ok(ExpertScore {
                expert_name: self.name().to_string(),
                weight: self.weight(),
                score: NO_LOCKFILE_SCORE,
                summary: "no dependency lockfile detected — dependency auditing skipped".to_string(),
                details,
                // `fallback = true` filters this placeholder out of the
                // weighted total (RENG-115, `experts::mod.rs:270-295`).
                // The row still surfaces — score 70, neutral summary,
                // `[hygiene]`-tagged note — so the reader sees "we
                // couldn't assess", but it no longer contributes to the
                // headline score.
                fallback: true,
                evaluated_loc: None,
                samples: None,
                ..Default::default()
            });
        }

        let total: usize = ecosystem_counts.iter().map(|e| e.total()).sum();
        let summary = build_summary(&ecosystem_counts, total);

        let score = if total > 200 {
            60
        } else if total > 100 {
            75
        } else if total > 50 {
            85
        } else {
            // 0..=50 deps across at least one recognised ecosystem.
            // Zero deps is a genuine measurement when a lockfile was
            // found (e.g. go.mod with no `require` block, like the
            // callme target), not a false claim — keep the score
            // high and let the summary name the ecosystem.
            95
        };

        if total > 200 {
            details.push(ScoreItem {
                severity: "medium".to_string(),
                message: format!(
                    "{} dependencies across {} — consider auditing for stale packages",
                    total,
                    ecosystem_names(&ecosystem_counts)
                ),
                file: None,
                recommendation: Some(ecosystem_specific_audit_recommendation(&ecosystem_counts)),
                effort: Some("medium".to_string()),
                ..Default::default()
            });
        }

        Ok(ExpertScore {
            expert_name: self.name().to_string(),
            weight: self.weight(),
            score,
            summary,
            details,
            fallback: false,
            evaluated_loc: None,
            samples: None,
            ..Default::default()
        })
    }
}

/// Count dependencies declared in a Cargo lockfile.
///
/// Each package entry starts with `[[package]]` and the next non-blank
/// line is `name = "<crate>"`. We count the `name = ` lines (one per
/// package), matching the legacy behaviour so the historical
/// `>200 → 60` audit threshold is preserved.
fn count_cargo_lock(content: &str) -> usize {
    content.lines().filter(|l| l.trim().starts_with("name = ")).count()
}

/// Count dependencies declared in a Go module file (`go.mod`).
///
/// `go.mod` declares dependencies two ways: a single-line form
/// (`require module/path v1.2.3`) and the block form that opens with
/// `require (` and closes with `)`. Both forms carry an optional
/// `// indirect` marker that is metadata, not a separate dep. Comments
/// inside the block (`// …`) and blank lines are skipped. Leading
/// whitespace is tolerated on every line so an indented `require (`
/// inside a build-script wrapper still parses.
fn count_go_dependencies(content: &str) -> usize {
    let mut in_block = false;
    let mut count = 0usize;
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("require ") {
            // Block opener: `require (` ends with `(` after the
            // keyword + space; we set the flag and fall through.
            if rest.starts_with('(') {
                in_block = true;
                continue;
            }
            // Single-line require: `require module/path v1.2.3`
            // Anything non-empty after `require ` (not `(`) is one
            // dependency.
            if !rest.is_empty() {
                count += 1;
            }
            continue;
        }
        if in_block {
            if trimmed == ")" {
                in_block = false;
                continue;
            }
            if trimmed.is_empty() || trimmed.starts_with("//") {
                continue;
            }
            // `module/path v1.2.3 // indirect` is still one entry;
            // the `// indirect` suffix is metadata, not a separate
            // dep. Trimmed it does not start with `//`, so we count.
            count += 1;
        }
    }
    count
}

/// Count dependencies declared in `package-lock.json`.
///
/// npm v1 lockfiles use a top-level `dependencies` map (direct deps).
/// npm v2 / v3 lockfiles use a top-level `packages` map that lists
/// every package in the dependency tree, keyed by install path. The
/// v2/v3 `packages` map **always carries an empty-string root key**
/// (`""`) holding the project itself, which would otherwise inflate the
/// count by one (a real v3 lockfile in this repo has 327 `packages`
/// entries with `""` = the `frontend` root package → 326 dependencies).
/// We filter the empty key out so the count is "actual deps", not
/// "rows in the JSON object". When the file is JSON-parseable AND
/// carries either map we return its (root-filtered) size; when it
/// parses but carries neither (e.g. a stripped or placeholder
/// lockfile), we return `None` so the caller treats the lockfile as
/// if it had no measurable deps — better to honestly say "we found
/// the file but couldn't read a count from it" than to pretend zero.
fn count_npm_dependencies(content: &str) -> Option<usize> {
    let json: serde_json::Value = serde_json::from_str(content).ok()?;
    if let Some(packages) = json.get("packages").and_then(|v| v.as_object()) {
        return Some(packages.keys().filter(|k| !k.is_empty()).count());
    }
    if let Some(deps) = json.get("dependencies").and_then(|v| v.as_object()) {
        return Some(deps.len());
    }
    None
}

/// Count packages declared in a Poetry lockfile (`poetry.lock`).
///
/// Each package is wrapped in a `[[package]]` TOML array marker; the
/// block below it carries name / version / dependencies. Counting
/// the markers is reliable across versions and across direct / transitive
/// distinction (poetry.lock resolves both into `[[package]]` blocks),
/// matching the npm `packages` map philosophy of "count what is
/// actually resolved".
fn count_poetry_dependencies(content: &str) -> usize {
    content.lines().filter(|l| l.trim() == "[[package]]").count()
}

/// Count pins declared in `Package.resolved` (SwiftPM).
///
/// SwiftPM ships two `Package.resolved` schemas:
///
/// - **v1** (legacy, pre-Swift-5.6) — the entire dependency graph is
///   nested inside a top-level `object` key, with the `pins` array
///   living at `object.pins`. Walking to `.pins` directly would miss
///   every v1 lockfile and silently report `SwiftPM (0)`.
/// - **v2 / v3** (modern, `version: 2`) — the `pins` array sits at
///   the top level, each element carrying `identity` / `state` /
///   `location` fields.
///
/// We try `json.pointer("/object/pins")` first (v1) and fall back to
/// `json.get("pins")` (v2/v3) so both shapes are counted correctly.
/// Unparseable bodies fail open to `0` rather than panicking.
fn count_swiftpm_dependencies(content: &str) -> usize {
    let json: serde_json::Value = match serde_json::from_str(content) {
        Ok(v) => v,
        Err(_) => return 0,
    };
    json.pointer("/object/pins")
        .or_else(|| json.get("pins"))
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0)
}

/// Render the per-ecosystem summary line.
///
/// Shape: `"<total> dependencies across <EcosystemA> (X) + <EcosystemB> (Y)"`.
/// When a single ecosystem has multiple lockfiles the parens render as
/// `"12 + 8"` (e.g. `npm (12 + 8)`) instead of duplicating the label
/// (`"npm (12) + npm (8)"`). The ecosystem name and the count are
/// kept together so the reader can tell at a glance whether the total
/// is dominated by one of them — and so the summary never names an
/// ecosystem that wasn't actually detected.
fn build_summary(ecosystem_counts: &[EcosystemCount], total: usize) -> String {
    let parts: Vec<String> = ecosystem_counts
        .iter()
        .map(|e| format!("{} ({})", e.ecosystem, e.render_counts()))
        .collect();
    format!("{} dependencies across {}", total, parts.join(" + "))
}

/// Join the detected ecosystem labels with `+`, for use in the >200
/// audit finding's headline (e.g. "across Cargo + npm").
fn ecosystem_names(ecosystem_counts: &[EcosystemCount]) -> String {
    ecosystem_counts
        .iter()
        .map(|e| e.ecosystem)
        .collect::<Vec<_>>()
        .join(" + ")
}

/// Pick the right auditing tool for each detected ecosystem and join
/// them with ` / ` in the recommendation string.
///
/// The recommendation names the canonical auditing tool per
/// ecosystem so a Go-shaped repo no longer gets a `cargo audit` hint
/// that has nothing to audit. Each detected ecosystem contributes
/// exactly one tool hint regardless of how many lockfiles it had —
/// the coalesce pass above already merged same-ecosystem lockfiles
/// into a single entry. If an ecosystem we don't recognise slips in
/// (a future ecosystem added without an audit hint) the
/// recommendation stays silent about it rather than naming the wrong
/// tool.
fn ecosystem_specific_audit_recommendation(ecosystem_counts: &[EcosystemCount]) -> String {
    let mut hints = Vec::new();
    for e in ecosystem_counts {
        match e.ecosystem {
            "Cargo" => hints.push("cargo audit"),
            "Go" => hints.push("govulncheck"),
            "npm" => hints.push("npm audit"),
            "Poetry" => hints.push("pip-audit (or `poetry export | safety check`)"),
            "SwiftPM" => hints.push("swift package audit"),
            _ => {}
        }
    }
    format!(
        "Run {} for known vulnerabilities and update stale or duplicate dependencies.",
        hints.join(" / ")
    )
}

/// Recommend the lockfile to add when none was detected, picked from
/// the dominant ecosystem markers in the repo. We can't be
/// authoritative (no lockfile = no canonical record), so the hint is
/// a best-effort signal: a repo full of `.go` files gets the
/// `govulncheck` hint rather than the `cargo audit` one.
fn no_lockfile_audit_hint(entries: &[crate::repo::FileEntry]) -> &'static str {
    let has_go = entries.iter().any(|e| {
        e.path.ends_with(".go")
            || e.path == "go.mod"
            || e.path == "go.sum"
            || e.path.ends_with("/go.mod")
            || e.path.ends_with("/go.sum")
    });
    if has_go {
        return "Add a go.mod (or run `go mod init`) — `go mod tidy` + `govulncheck` can then audit declared dependencies.";
    }
    let has_cargo = entries.iter().any(|e| {
        e.path.ends_with(".rs")
            || e.path.ends_with("Cargo.toml")
            || e.path.ends_with("Cargo.lock")
            || e.path.ends_with("/Cargo.toml")
            || e.path.ends_with("/Cargo.lock")
    });
    if has_cargo {
        return "Commit Cargo.lock — `cargo audit` can then review dependencies for known issues.";
    }
    let has_npm = entries.iter().any(|e| {
        e.path.ends_with("package.json")
            || e.path.ends_with("package-lock.json")
            || e.path.ends_with("yarn.lock")
            || e.path.ends_with("pnpm-lock.yaml")
            || e.path.ends_with("/package.json")
            || e.path.ends_with("/package-lock.json")
    });
    if has_npm {
        return "Commit package-lock.json — `npm audit` can then review dependencies for known vulnerabilities.";
    }
    let has_python = entries.iter().any(|e| {
        e.path.ends_with(".py")
            || e.path == "pyproject.toml"
            || e.path == "Pipfile"
            || e.path == "poetry.lock"
            || e.path.ends_with("/pyproject.toml")
            || e.path.ends_with("/poetry.lock")
    });
    if has_python {
        return "Commit poetry.lock (or generate `pip-audit`'s requirements input) — dependency auditing can then review the resolved set.";
    }
    "Add a recognised lockfile (Cargo.lock / go.mod / package-lock.json / poetry.lock / Package.resolved) so dependency health can be audited."
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::{experts::RepoExpert, FileEntry, RepoStats};

    fn ctx_with(entries: Vec<FileEntry>) -> crate::repo::experts::RepoContext {
        let stats = RepoStats {
            total_files: entries.len(),
            total_loc: entries.iter().map(|e| e.loc).sum(),
            ..Default::default()
        };
        crate::repo::experts::RepoContext {
            entries,
            stats,
            llm_configs: Vec::new(),
            config: None,
            facts_block: None,
        }
    }

    /// Write a fixture file inside `dir` and return a `FileEntry`
    /// pointing at it, so the expert sees a real path it can read.
    fn write_entry(dir: &tempfile::TempDir, relative: &str, body: &str, language: &str) -> FileEntry {
        let path = dir.path().join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(&path, body).expect("write fixture");
        FileEntry {
            path: path.to_string_lossy().into_owned(),
            language: language.to_string(),
            loc: body.lines().count(),
            is_binary: false,
            is_generated: false,
        }
    }

    /// Build a Cargo.lock fixture holding `n` packages.
    fn cargo_lock_with(n: usize) -> String {
        let mut s = String::from("version = 3\n\n");
        for i in 0..n {
            s.push_str(&format!("[[package]]\nname = \"pkg{i}\"\nversion = \"0.1.0\"\n\n"));
        }
        s
    }

    // ─── Counting helpers ────────────────────────

    #[test]
    fn count_cargo_lock_counts_each_name_line() {
        let body = cargo_lock_with(7);
        assert_eq!(count_cargo_lock(&body), 7);
    }

    #[test]
    fn count_go_dependencies_handles_block_form_and_indirection() {
        // Mixed shape: single-line require, block-form requires with
        // an `// indirect` marker, an empty block, a comment-only line,
        // and a blank line inside a block. The expected count is the
        // number of dependency entries — not the number of source lines.
        let body = "\
module example.com/m

go 1.27

require github.com/x/y v1.2.3

require (
\tgithub.com/a/b v2.0.0
\tgithub.com/c/d v0.1.0 // indirect
\t// a free comment line
\tgithub.com/e/f v3.0.0
)

require (
)
";
        assert_eq!(
            count_go_dependencies(body),
            4,
            "single-line (1) + block (3) + empty block (0) = 4"
        );
    }

    #[test]
    fn count_go_dependencies_zero_when_go_mod_has_no_requires() {
        // The callme reproducer shape: a real `go.mod` with only
        // `module` and `go` directives. We must not invent deps from
        // those directive lines.
        let body = "module example.com/m\n\ngo 1.27\n";
        assert_eq!(count_go_dependencies(body), 0);
    }

    #[test]
    fn count_npm_dependencies_reads_packages_map_in_v2_v3_lockfiles() {
        // The v2/v3 `packages` map always carries a root entry under
        // the `""` key holding the project itself. The counter must
        // exclude that root key (the project itself is not its own
        // dependency) and report only the actual deps.
        let v3 = r#"{
            "lockfileVersion": 3,
            "packages": {
                "": { "name": "root" },
                "node_modules/foo": { "version": "1.0.0" },
                "node_modules/bar": { "version": "2.0.0" }
            }
        }"#;
        assert_eq!(
            count_npm_dependencies(v3),
            Some(2),
            "root key '' must be excluded — count is actual deps, not JSON rows"
        );
    }

    #[test]
    fn count_npm_dependencies_excludes_root_even_with_many_deps() {
        // Real-world shape: hundreds of `packages` entries plus the
        // root. The root must not be counted, so the answer is
        // entries-without-root rather than entries-including-root.
        let mut body = String::from(r#"{"lockfileVersion":3,"packages":{"":{"name":"root"}"#);
        for i in 0..99 {
            body.push_str(&format!(r#","node_modules/pkg{i}":{{"version":"1.0.0"}}"#));
        }
        body.push_str("}}");
        assert_eq!(count_npm_dependencies(&body), Some(99));
    }

    #[test]
    fn count_npm_dependencies_falls_back_to_dependencies_map_in_v1() {
        let v1 = r#"{
            "lockfileVersion": 1,
            "dependencies": {
                "foo": { "version": "1.0.0" },
                "bar": { "version": "2.0.0" }
            }
        }"#;
        assert_eq!(count_npm_dependencies(v1), Some(2));
    }

    #[test]
    fn count_npm_dependencies_returns_none_when_lockfile_has_no_countable_map() {
        // JSON-parseable but with neither `packages` nor
        // `dependencies` — an exotic or stripped shape. Treat as "no
        // measurement available" rather than 0, so the caller does
        // not silently drop the expert.
        let stub = r#"{ "lockfileVersion": 3 }"#;
        assert_eq!(count_npm_dependencies(stub), None);
    }

    #[test]
    fn count_poetry_dependencies_counts_package_markers() {
        let body = "\
[[package]]
name = \"a\"
version = \"1.0.0\"

[[package]]
name = \"b\"
version = \"2.0.0\"

[[package]]
name = \"c\"
version = \"0.5.0\"
";
        assert_eq!(count_poetry_dependencies(body), 3);
    }

    #[test]
    fn count_swiftpm_dependencies_reads_top_level_pins_array() {
        // SwiftPM v2 / v3 (modern): top-level `pins` array.
        let body = r#"{
            "version": 2,
            "pins": [
                { "package": "a" },
                { "package": "b" }
            ]
        }"#;
        assert_eq!(count_swiftpm_dependencies(body), 2);
    }

    #[test]
    fn count_swiftpm_dependencies_reads_v1_nested_object_pins() {
        // SwiftPM v1 (legacy, pre-Swift-5.6): the entire dependency
        // graph is nested under `object`, with `pins` living at
        // `object.pins`. The v1 lockfile would otherwise report
        // `SwiftPM (0)` — the exact false-zero this round fixes.
        let body = r#"{
            "version": 1,
            "object": {
                "pins": [
                    {
                        "package": "a",
                        "repositoryURL": "https://example.com/a",
                        "state": { "version": "1.0.0" }
                    },
                    {
                        "package": "b",
                        "repositoryURL": "https://example.com/b",
                        "state": { "version": "2.0.0" }
                    },
                    {
                        "package": "c",
                        "repositoryURL": "https://example.com/c",
                        "state": { "version": "3.0.0" }
                    }
                ]
            }
        }"#;
        assert_eq!(
            count_swiftpm_dependencies(body),
            3,
            "v1 Package.resolved must count pins under object.pins"
        );
    }

    #[test]
    fn count_swiftpm_dependencies_zero_on_unparseable_body() {
        // Fail-open: a malformed Package.resolved must not panic and
        // must not be promoted to a phantom dependency count.
        assert_eq!(count_swiftpm_dependencies("not json"), 0);
    }

    // ─── End-to-end expert.evaluate ──────────────

    #[tokio::test]
    async fn cargo_only_repo_reports_cargo_count_in_summary() {
        // 30 packages → score 95 (≤50). Summary names Cargo, not Go.
        let dir = tempfile::tempdir().unwrap();
        let entry = write_entry(&dir, "Cargo.lock", &cargo_lock_with(30), "Config");
        let score = Dependency.evaluate(&ctx_with(vec![entry]), None).await.unwrap();
        assert_eq!(score.score, 95);
        assert!(
            score.summary.contains("Cargo (30)"),
            "summary must name Cargo and the count: {}",
            score.summary
        );
        assert!(
            !score.summary.contains("Go"),
            "summary must not mention Go when no Go lockfile is present: {}",
            score.summary
        );
        assert!(score.details.is_empty());
    }

    #[tokio::test]
    async fn cargo_above_200_triggers_audit_with_cargo_recommendation() {
        // The legacy `>200 → 60` audit threshold is preserved and
        // tagged with the ecosystem-specific `cargo audit` hint.
        let dir = tempfile::tempdir().unwrap();
        let entry = write_entry(&dir, "Cargo.lock", &cargo_lock_with(201), "Config");
        let score = Dependency.evaluate(&ctx_with(vec![entry]), None).await.unwrap();
        assert_eq!(score.score, 60, ">200 → 60: {}", score.summary);
        assert_eq!(score.details.len(), 1);
        let detail = &score.details[0];
        assert!(
            detail.message.contains("201"),
            "detail must cite the total count: {}",
            detail.message
        );
        assert!(
            detail.recommendation.as_deref().unwrap_or("").contains("cargo audit"),
            "Cargo recommendation must name cargo audit: {:?}",
            detail.recommendation
        );
    }

    #[tokio::test]
    async fn go_only_zero_dep_repo_reports_zero_in_summary_no_false_cargo_claim() {
        // The RENG-118 reproducer (callme): a real `go.mod` with no
        // `require` block. The summary must say Go (not Cargo) and
        // the score must be high — 0 deps is a genuine measurement
        // here, not a false claim.
        let dir = tempfile::tempdir().unwrap();
        let body = "module example.com/m\n\ngo 1.27\n";
        let entry = write_entry(&dir, "go.mod", body, "Config");
        let score = Dependency.evaluate(&ctx_with(vec![entry]), None).await.unwrap();
        assert_eq!(
            score.score, 95,
            "0 deps in a recognised lockfile is high: {}",
            score.summary
        );
        assert!(
            score.summary.contains("Go (0)"),
            "summary must name Go (0): {}",
            score.summary
        );
        assert!(
            !score.summary.contains("Cargo"),
            "summary must NEVER mention Cargo when no Cargo.lock is present: {}",
            score.summary
        );
        assert!(
            !score.summary.contains("0 dependencies from Cargo"),
            "the exact legacy false-positive string must be gone: {}",
            score.summary
        );
        assert!(score.details.is_empty());
    }

    #[tokio::test]
    async fn npm_repo_reports_npm_count_in_summary() {
        // An npm v3 lockfile with 3 packages plus the root key.
        // The root key is excluded by the npm v2/v3 counter
        // (real-world v3 lockfiles always include it), so the
        // reported count is 3 (the actual deps), not 4 (entries
        // in the JSON object).
        let dir = tempfile::tempdir().unwrap();
        let body = r#"{
            "lockfileVersion": 3,
            "packages": {
                "": { "name": "root" },
                "node_modules/a": { "version": "1.0.0" },
                "node_modules/b": { "version": "1.0.0" },
                "node_modules/c": { "version": "1.0.0" }
            }
        }"#;
        let entry = write_entry(&dir, "package-lock.json", body, "Config");
        let score = Dependency.evaluate(&ctx_with(vec![entry]), None).await.unwrap();
        assert_eq!(score.score, 95);
        assert!(
            score.summary.contains("npm (3)"),
            "summary must name npm with its (corrected) count of 3 actual deps: {}",
            score.summary
        );
    }

    #[tokio::test]
    async fn multi_ecosystem_repo_reports_each_ecosystem_with_its_count() {
        // Both Cargo.lock (5 packages) and go.mod (3 deps) present:
        // the summary must name BOTH ecosystems and the score must
        // follow the total (8 → 95).
        let dir = tempfile::tempdir().unwrap();
        let cargo = write_entry(&dir, "Cargo.lock", &cargo_lock_with(5), "Config");
        let gomod = write_entry(
            &dir,
            "go.mod",
            "module example.com/m\n\ngo 1.27\n\nrequire (\n\tgithub.com/a/b v1.0.0\n\tgithub.com/c/d v2.0.0\n\tgithub.com/e/f v0.5.0\n)\n",
            "Config",
        );
        let score = Dependency.evaluate(&ctx_with(vec![cargo, gomod]), None).await.unwrap();
        assert_eq!(score.score, 95, "8 deps total → 95: {}", score.summary);
        assert!(
            score.summary.contains("Cargo (5)"),
            "summary must list Cargo with 5: {}",
            score.summary
        );
        assert!(
            score.summary.contains("Go (3)"),
            "summary must list Go with 3: {}",
            score.summary
        );
    }

    #[tokio::test]
    async fn no_lockfile_emits_neutral_summary_and_hygiene_note() {
        // The defect RENG-118 was filed for. A repo with no recognised
        // lockfile must NOT score 100 with a misleading "0
        // dependencies from Cargo.lock" claim — it gets a neutral
        // score, an explicit summary, and a [hygiene]-tagged note.
        // RENG-115 / M62-r1: the 70 must also carry `fallback = true`
        // so the row leaves the weighted total instead of contributing
        // a placeholder "70/100" to the headline aggregate.
        let dir = tempfile::tempdir().unwrap();
        // Source files but no lockfile / manifest the expert reads.
        let rs = write_entry(&dir, "src/main.rs", "fn main() {}\n", "Rust");
        let score = Dependency.evaluate(&ctx_with(vec![rs]), None).await.unwrap();
        assert_eq!(score.score, 70, "no-lockfile → neutral 70 (not 100): {}", score.summary);
        assert!(
            score.summary.contains("no dependency lockfile detected"),
            "summary must be the explicit no-lockfile message: {}",
            score.summary
        );
        assert!(
            !score.summary.contains("Cargo"),
            "summary must not mention Cargo when there is no lockfile: {}",
            score.summary
        );
        // RENG-115 placeholder contract: the no-lockfile path must be
        // marked as a fallback so the weighted-total math filters it
        // out (otherwise a "dependency 70/100 (medium)" row would
        // contribute ~9 points to the headline score on a
        // lockfile-less repo — the exact bug M62-r1 caught live).
        assert!(
            score.fallback,
            "no-lockfile placeholder must be flagged `fallback = true` \
             so the weighted total filters it out (RENG-115, \
             `experts::mod.rs:270-295`): score={} summary={}",
            score.score, score.summary
        );
        assert_eq!(
            score.details.len(),
            1,
            "exactly one hygiene finding: {:?}",
            score.details.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
        let detail = &score.details[0];
        assert_eq!(detail.severity, "note");
        assert!(
            detail.message.contains("[hygiene]"),
            "no-lockfile finding must carry the [hygiene] tag: {}",
            detail.message
        );
        assert!(
            detail.recommendation.is_some(),
            "no-lockfile finding must say what to do: {}",
            detail.message
        );
        // Rust markers → the hint must mention cargo, NOT Go/npm.
        let rec = detail.recommendation.as_deref().unwrap();
        assert!(
            rec.contains("Cargo"),
            "Rust-only repo should get the Cargo-aware hint: {rec}"
        );
    }

    #[tokio::test]
    async fn no_lockfile_go_marker_recommends_govulncheck_not_cargo() {
        // Same neutral shape, but the repo has Go source files. The
        // hygiene reminder must recommend govulncheck instead of
        // cargo audit. Pinned `fallback = true` for the same reason
        // as the Rust variant above — the placeholder must not enter
        // the weighted total.
        let dir = tempfile::tempdir().unwrap();
        let go = write_entry(&dir, "main.go", "package main\nfunc main() {}\n", "Go");
        let score = Dependency.evaluate(&ctx_with(vec![go]), None).await.unwrap();
        assert_eq!(score.score, 70, "{}", score.summary);
        assert!(
            score.fallback,
            "no-lockfile placeholder must be flagged fallback=true: {}",
            score.summary
        );
        let rec = score.details[0].recommendation.as_deref().unwrap_or_default();
        assert!(
            rec.contains("govulncheck") || rec.contains("go.mod"),
            "Go-only repo should get the Go-aware hint: {rec}"
        );
    }

    #[test]
    fn coalesce_ecosystem_counts_folds_same_ecosystem_lockfiles() {
        // Two npm lockfiles in a monorepo: the duplicate-label
        // regression M62-r1 flagged. Without coalescing they would
        // render as "npm (12) + npm (8)" — confusing, and the reader
        // would have to dedupe by eye. After coalescing they read as
        // a single "npm (12 + 8)" label with total 20.
        let input = vec![
            EcosystemCount {
                ecosystem: "npm",
                counts: vec![12],
            },
            EcosystemCount {
                ecosystem: "Cargo",
                counts: vec![5],
            },
            EcosystemCount {
                ecosystem: "npm",
                counts: vec![8],
            },
        ];
        let merged = coalesce_ecosystem_counts(input);
        assert_eq!(merged.len(), 2, "two distinct ecosystems after coalesce");
        // BTreeMap ordering = Cargo then npm.
        assert_eq!(merged[0].ecosystem, "Cargo");
        assert_eq!(merged[0].counts, vec![5]);
        assert_eq!(merged[1].ecosystem, "npm");
        assert_eq!(merged[1].counts, vec![12, 8]);
        assert_eq!(merged[1].total(), 20);
        assert_eq!(merged[1].render_counts(), "12 + 8");
    }

    #[tokio::test]
    async fn monorepo_with_two_npm_lockfiles_renders_one_label() {
        // End-to-end: a single monorepo with two `package-lock.json`
        // files. The summary must render a single `"npm (X + Y)"`
        // label rather than two repeated `"npm (X) + npm (Y)"`
        // labels. Tests the bonus non-blocking observation M62-r1
        // listed (duplicate ecosystem labels).
        let dir = tempfile::tempdir().unwrap();
        let l1 = write_entry(
            &dir,
            "frontend/package-lock.json",
            r#"{"lockfileVersion":3,"packages":{"":{"name":"f"},"node_modules/a":{"version":"1.0.0"}}}"#,
            "Config",
        );
        let l2 = write_entry(
            &dir,
            "backend/package-lock.json",
            r#"{"lockfileVersion":3,"packages":{"":{"name":"b"},"node_modules/x":{"version":"1.0.0"},"node_modules/y":{"version":"2.0.0"}}}"#,
            "Config",
        );
        let score = Dependency.evaluate(&ctx_with(vec![l1, l2]), None).await.unwrap();
        // 1 + 2 = 3 actual deps (root key filtered).
        assert!(
            score.summary.contains("npm (1 + 2)"),
            "summary must show a single npm label with the per-lockfile counts combined: {}",
            score.summary
        );
        assert!(
            !score.summary.contains("npm (1) + npm"),
            "summary must NOT duplicate the npm label: {}",
            score.summary
        );
    }

    #[tokio::test]
    async fn entry_loop_skips_non_lockfile_entries_without_reading_them() {
        // Cheap non-blocking observation M62-r1 listed: the entry
        // loop now checks the basename first and only reads files
        // whose name matches one of the five lockfile basenames.
        // A repo whose scanned entries are mostly non-lockfiles
        // must still produce the right summary.
        let dir = tempfile::tempdir().unwrap();
        let big_rs = write_entry(&dir, "src/lib.rs", &("// many source lines\n".repeat(500)), "Rust");
        let small_rs = write_entry(&dir, "src/main.rs", "fn main() {}\n", "Rust");
        let gomod = write_entry(
            &dir,
            "go.mod",
            "module example.com/m\n\ngo 1.27\n\nrequire (\n\tgithub.com/a/b v1.0.0\n\tgithub.com/c/d v2.0.0\n)\n",
            "Config",
        );
        let entries = vec![big_rs, small_rs, gomod];
        let score = Dependency.evaluate(&ctx_with(entries), None).await.unwrap();
        assert!(
            score.summary.contains("Go (2)"),
            "go.mod is read, the two .rs source files are skipped: {}",
            score.summary
        );
    }
}
