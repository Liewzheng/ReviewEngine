//! Deterministic repository facts ("fact grounding").
//!
//! Values here are computed statically from scan entries and injected into
//! LLM prompts by other units, so models cannot contradict observable
//! reality — e.g. reporting "missing type hints" on a fully annotated
//! Python codebase. Everything is cheap, deterministic, and parser-free.
//!
//! Python annotation statistics use line-level heuristics. Known
//! limitations:
//!
//! - A `def` is any trimmed line starting with `def ` or `async def `; a
//!   match inside a string or comment would be counted (rare in practice).
//! - Multi-line signatures are accumulated until a line ending with `:`,
//!   capped at 64 lines as a guard against malformed input.
//! - Parameters are split on top-level commas with bracket/quote tracking;
//!   triple-quoted default values can still confuse the splitter.
//! - `self`/`cls` need no annotation (mypy does not require it either),
//!   and zero-parameter functions count as fully annotated.

use std::fmt::Write as _;
use std::path::Path;

use crate::repo::FileEntry;

/// Deterministic, statically computed facts about a repository.
///
/// Coverage ratios are `None` when no Python `def` was found — the ratio
/// is undefined, not zero. Consumers must not treat "no Python" as "no
/// annotations".
#[derive(Debug, Clone, Default)]
pub struct RepoFacts {
    /// Total number of Python `def` statements analysed (0 when no Python).
    pub python_def_total: usize,
    /// Number of Python defs with a return-type annotation (`->`).
    pub python_return_annotated: usize,
    /// Number of Python defs whose parameters are all type-annotated.
    pub python_fully_param_annotated: usize,
    /// `python_return_annotated / python_def_total` in `[0.0, 1.0]`;
    /// `None` when no Python defs were found.
    pub python_return_annotation_coverage: Option<f64>,
    /// `python_fully_param_annotated / python_def_total` in `[0.0, 1.0]`;
    /// `None` when no Python defs were found.
    pub python_full_param_annotation_coverage: Option<f64>,
    /// CI configuration files detected among the entries (paths as given
    /// in the entries, sorted and deduplicated).
    pub ci_configs: Vec<String>,
    /// Entries matching test naming conventions (see [`is_test_file`]).
    pub test_files: usize,
    /// Non-test, non-binary, non-generated entries.
    pub source_files: usize,
}

impl RepoFacts {
    /// Render the facts as a compact YAML block for prompt injection.
    ///
    /// Field order is stable; coverage ratios are rendered with two
    /// decimals; `python: null` means no Python defs were found;
    /// `ci_configs: []` means no CI configuration was detected.
    pub fn to_prompt_block(&self) -> String {
        let mut out = String::from("repo_facts:\n");
        if let (Some(ret), Some(param)) = (
            self.python_return_annotation_coverage,
            self.python_full_param_annotation_coverage,
        ) {
            let _ = writeln!(out, "  python:");
            let _ = writeln!(out, "    def_total: {}", self.python_def_total);
            let _ = writeln!(out, "    return_annotated: {}", self.python_return_annotated);
            let _ = writeln!(out, "    return_annotation_coverage: {ret:.2}");
            let _ = writeln!(out, "    fully_param_annotated: {}", self.python_fully_param_annotated);
            let _ = writeln!(out, "    full_param_annotation_coverage: {param:.2}");
        } else {
            let _ = writeln!(out, "  python: null");
        }
        if self.ci_configs.is_empty() {
            let _ = writeln!(out, "  ci_configs: []");
        } else {
            let _ = writeln!(out, "  ci_configs:");
            for c in &self.ci_configs {
                let _ = writeln!(out, "    - \"{c}\"");
            }
        }
        let _ = writeln!(out, "  test_files: {}", self.test_files);
        let _ = writeln!(out, "  source_files: {}", self.source_files);
        out
    }
}

/// Compute repository facts from scan entries.
///
/// CI / test / source classification uses entry metadata only; Python
/// annotation statistics additionally read file contents (unreadable
/// files are skipped, matching the other static experts). The CI check
/// matches on entry paths only — the scanner's hidden-file whitelist is
/// deliberately not duplicated here.
pub fn compute(entries: &[FileEntry]) -> RepoFacts {
    let mut facts = RepoFacts::default();
    let mut ci = std::collections::BTreeSet::new();

    for entry in entries {
        if entry.is_binary || entry.is_generated {
            continue;
        }
        if is_ci_config(&entry.path) {
            ci.insert(entry.path.clone());
        }
        let name = Path::new(&entry.path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        if is_test_file(name, &entry.path) {
            facts.test_files += 1;
        } else {
            facts.source_files += 1;
        }
        if entry.language == "Python" {
            if let Ok(content) = std::fs::read_to_string(&entry.path) {
                let stats = analyse_python_defs(&content);
                facts.python_def_total += stats.total;
                facts.python_return_annotated += stats.return_annotated;
                facts.python_fully_param_annotated += stats.fully_param_annotated;
            }
        }
    }
    facts.ci_configs = ci.into_iter().collect();

    if facts.python_def_total > 0 {
        let total = facts.python_def_total as f64;
        facts.python_return_annotation_coverage = Some(facts.python_return_annotated as f64 / total);
        facts.python_full_param_annotation_coverage = Some(facts.python_fully_param_annotated as f64 / total);
    }
    facts
}

/// Whether a file looks like a test file by naming convention.
///
/// Language-agnostic heuristics, shared with the test-coverage expert and
/// the code-organization large-file statistic so all of them classify
/// exactly the same files:
///   Rust:    `*_test.rs`, `tests/*.rs`, sibling `tests.rs` / `*_tests.rs`
///            (the `#[cfg(test)] mod tests;` split-file convention)
///   Python:  `test_*.py`, `*_test.py`, `tests/*.py`
///   JS/TS:   `*.test.js`, `*.spec.js`, `*.test.ts`, `*.spec.ts`, `__tests__/*`
///   Go:      `*_test.go`
///   Java:    `*Test.java`, `src/test/*`
///   Swift:   `*Tests.swift`, `Tests/*` (SwiftPM)
///
/// The `.test.` / `.spec.` / `_test.` infixes match in ANY extension
/// (e.g. `foo.test.tsx`, `bar.spec.vue`, `baz_test.ts`), not just the
/// per-language examples listed above — the convention is the infix,
/// not the file type.
pub(crate) fn is_test_file(name: &str, path: &str) -> bool {
    name.contains("_test.")
        || name == "tests.rs"
        || name.ends_with("_tests.rs")
        || name.starts_with("test_")
        || name.contains(".test.")
        || name.contains(".spec.")
        || name.ends_with("Test.java")
        || name.ends_with("Tests.swift")
        || path.contains("/tests/")
        || path.contains("/Tests/")
        || path.starts_with("tests/")
        || path.contains("__tests__")
        || path.contains("/test/")
        || path.starts_with("test/")
        || path.contains("/spec/")
}

/// Whether `path` is a well-known CI configuration file.
///
/// Matches on file name / directory location only; entries have already
/// passed the scanner's filtering, so no whitelist logic is duplicated.
fn is_ci_config(path: &str) -> bool {
    let name = Path::new(path).file_name().and_then(|n| n.to_str()).unwrap_or("");
    let norm = path.replace('\\', "/");
    matches!(
        name,
        ".gitlab-ci.yml"
            | ".gitlab-ci.yaml"
            | "Jenkinsfile"
            | ".travis.yml"
            | "azure-pipelines.yml"
            | "bitbucket-pipelines.yml"
            | "appveyor.yml"
            | ".drone.yml"
    ) || norm.starts_with(".github/workflows/")
        || norm.contains("/.github/workflows/")
        || norm.starts_with(".circleci/")
        || norm.contains("/.circleci/")
        || norm.starts_with(".buildkite/")
        || norm.contains("/.buildkite/")
}

/// Running totals for Python `def` signatures in one file.
#[derive(Default)]
struct DefStats {
    total: usize,
    return_annotated: usize,
    fully_param_annotated: usize,
}

/// Line-level scan for `def` signatures; see the module docs for the
/// heuristic's known limitations.
fn analyse_python_defs(content: &str) -> DefStats {
    let mut stats = DefStats::default();
    let mut sig = String::new();
    let mut in_sig = false;
    let mut sig_lines = 0usize;

    for line in content.lines() {
        let trimmed = line.trim();
        if in_sig {
            sig.push(' ');
            sig.push_str(trimmed);
            sig_lines += 1;
            // A signature ends at the line whose trailing ':' opens the body.
            if trimmed.ends_with(':') || sig_lines >= 64 {
                record_signature(&sig, &mut stats);
                in_sig = false;
                sig.clear();
            }
            continue;
        }
        if trimmed.starts_with("def ") || trimmed.starts_with("async def ") {
            if trimmed.ends_with(':') {
                record_signature(trimmed, &mut stats);
            } else {
                in_sig = true;
                sig.clear();
                sig.push_str(trimmed);
                sig_lines = 1;
            }
        }
    }
    // Unterminated signature at EOF (malformed input): still count it.
    if in_sig {
        record_signature(&sig, &mut stats);
    }
    stats
}

/// Update `stats` with one accumulated signature.
fn record_signature(sig: &str, stats: &mut DefStats) {
    stats.total += 1;

    // Return annotation: `->` between the final `)` and the trailing `:`.
    let body = sig.trim_end();
    let body = body.strip_suffix(':').unwrap_or(body);
    if let Some(close) = body.rfind(')') {
        if body[close + 1..].contains("->") {
            stats.return_annotated += 1;
        }
    }

    // Parameter list: between the first `(` and the final `)`.
    let params = match (sig.find('('), sig.rfind(')')) {
        (Some(open), Some(close)) if open < close => &sig[open + 1..close],
        _ => "",
    };
    let mut all_annotated = true;
    for token in split_top_level_commas(params) {
        let token = token.trim();
        // Bare `*` / `/` are PEP 484/570 separator markers, not parameters.
        if token.is_empty() || token == "*" || token == "/" {
            continue;
        }
        let token = token.trim_start_matches('*').trim();
        if token.is_empty() {
            continue;
        }
        // Drop any default value before checking for the `name: type` colon.
        let head = token.split('=').next().unwrap_or(token).trim();
        if head == "self" || head == "cls" {
            continue;
        }
        if !head.contains(':') {
            all_annotated = false;
        }
    }
    // Zero-parameter functions are vacuously fully annotated.
    if all_annotated {
        stats.fully_param_annotated += 1;
    }
}

/// Split a parameter list on top-level commas, tracking bracket depth and
/// simple quotes. Triple-quoted strings are not tracked (documented limit).
fn split_top_level_commas(params: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut current = String::new();
    for ch in params.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        match ch {
            '\\' if quote.is_some() => {
                current.push(ch);
                escaped = true;
            }
            '\'' | '"' => {
                if quote == Some(ch) {
                    quote = None;
                } else if quote.is_none() {
                    quote = Some(ch);
                }
                current.push(ch);
            }
            '(' | '[' | '{' if quote.is_none() => {
                depth += 1;
                current.push(ch);
            }
            ')' | ']' | '}' if quote.is_none() => {
                depth -= 1;
                current.push(ch);
            }
            ',' if depth == 0 && quote.is_none() => parts.push(std::mem::take(&mut current)),
            _ => current.push(ch),
        }
    }
    if !current.trim().is_empty() {
        parts.push(current);
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, language: &str) -> FileEntry {
        FileEntry {
            path: path.to_string(),
            language: language.to_string(),
            loc: 10,
            is_binary: false,
            is_generated: false,
        }
    }

    fn write_py(dir: &tempfile::TempDir, name: &str, content: &str) -> String {
        let path = dir.path().join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent dir");
        }
        std::fs::write(&path, content).expect("write python fixture");
        path.to_string_lossy().into_owned()
    }

    /// Two fully annotated defs (one async).
    const ANNOTATED_PY: &str = r#"
def add(a: int, b: int) -> int:
    return a + b


async def fetch(url: str) -> bytes:
    return b""
"#;

    /// One unannotated def, one partially annotated multi-line def.
    const MIXED_PY: &str = r#"
def greet(name):
    return "hi " + name


def multi(
    a: int,
    b,
) -> str:
    return str(a) + b
"#;

    #[test]
    fn python_annotation_coverage_is_computed_per_def() {
        let dir = tempfile::tempdir().expect("tempdir");
        let annotated = write_py(&dir, "annotated.py", ANNOTATED_PY);
        let mixed = write_py(&dir, "mixed.py", MIXED_PY);
        let facts = compute(&[entry(&annotated, "Python"), entry(&mixed, "Python")]);

        assert_eq!(facts.python_def_total, 4);
        assert_eq!(facts.python_return_annotated, 3);
        assert_eq!(facts.python_fully_param_annotated, 2);
        assert_eq!(facts.python_return_annotation_coverage, Some(0.75));
        assert_eq!(facts.python_full_param_annotation_coverage, Some(0.5));
    }

    #[test]
    fn python_coverage_is_none_without_python() {
        let facts = compute(&[entry("src/main.rs", "Rust")]);
        assert_eq!(facts.python_def_total, 0);
        assert_eq!(facts.python_return_annotation_coverage, None);
        assert_eq!(facts.python_full_param_annotation_coverage, None);
    }

    #[test]
    fn nested_and_quoted_defaults_do_not_break_param_splitting() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_py(
            &dir,
            "nested.py",
            "def f(a: dict[str, int], b: str = \"x,y\") -> None:\n    pass\n",
        );
        let facts = compute(&[entry(&path, "Python")]);

        assert_eq!(facts.python_def_total, 1);
        assert_eq!(facts.python_return_annotated, 1);
        assert_eq!(facts.python_fully_param_annotated, 1);
        assert_eq!(facts.python_return_annotation_coverage, Some(1.0));
        assert_eq!(facts.python_full_param_annotation_coverage, Some(1.0));
    }

    #[test]
    fn ci_and_test_source_counts_come_from_entry_paths() {
        let facts = compute(&[
            entry(".gitlab-ci.yml", "Config"),
            entry(".github/workflows/ci.yml", "Config"),
            entry("src/app.py", "Python"),
            entry("tests/test_app.py", "Python"),
            entry("README.md", "Documentation"),
        ]);

        assert_eq!(
            facts.ci_configs,
            vec![".github/workflows/ci.yml".to_string(), ".gitlab-ci.yml".to_string()]
        );
        assert_eq!(facts.test_files, 1);
        // The .py files do not exist on disk here: no def stats, but the
        // entries still count towards the file totals.
        assert_eq!(facts.source_files, 4);
        assert_eq!(facts.python_def_total, 0);
    }

    #[test]
    fn prompt_block_snapshot_with_python_and_ci() {
        let facts = RepoFacts {
            python_def_total: 4,
            python_return_annotated: 3,
            python_fully_param_annotated: 2,
            python_return_annotation_coverage: Some(0.75),
            python_full_param_annotation_coverage: Some(0.5),
            ci_configs: vec![".github/workflows/ci.yml".to_string(), ".gitlab-ci.yml".to_string()],
            test_files: 1,
            source_files: 9,
        };
        let expected = r#"repo_facts:
  python:
    def_total: 4
    return_annotated: 3
    return_annotation_coverage: 0.75
    fully_param_annotated: 2
    full_param_annotation_coverage: 0.50
  ci_configs:
    - ".github/workflows/ci.yml"
    - ".gitlab-ci.yml"
  test_files: 1
  source_files: 9
"#;
        assert_eq!(facts.to_prompt_block(), expected);
    }

    #[test]
    fn prompt_block_snapshot_without_python_or_ci() {
        let facts = RepoFacts {
            test_files: 2,
            source_files: 7,
            ..Default::default()
        };
        let expected = r#"repo_facts:
  python: null
  ci_configs: []
  test_files: 2
  source_files: 7
"#;
        assert_eq!(facts.to_prompt_block(), expected);
    }

    #[test]
    fn is_test_file_recognizes_swiftpm_structure() {
        // SwiftPM convention: `*Tests.swift` files under the uppercase `Tests/`
        // directory (e.g. `Tests/FooTests/BarTests.swift`).
        assert!(is_test_file("BarTests.swift", "Tests/FooTests/BarTests.swift"));
        assert!(is_test_file("FooTests.swift", "Tests/FooTests/FooTests.swift"));
        // Scanned paths are absolute, so the `Tests/` directory segment also
        // flags files regardless of the `Tests.swift` suffix.
        assert!(is_test_file("FooSpec.swift", "/repo/Tests/FooTests/FooSpec.swift"));
        // Regular Swift source must not be flagged as a test.
        assert!(!is_test_file("main.swift", "Sources/main.swift"));
        // Lowercase `tests/` detection is unchanged (no regression).
        assert!(is_test_file("helpers.py", "/repo/tests/helpers.py"));
    }

    #[test]
    fn is_test_file_matches_documented_conventions() {
        let cases: &[(&str, &str, bool)] = &[
            ("foo_test.rs", "src/foo_test.rs", true),
            ("utils.rs", "src/utils.rs", false),
            ("test_utils.py", "src/test_utils.py", true),
            ("utils.py", "src/utils.py", false),
            ("app.test.ts", "src/app.test.ts", true),
            ("utils.spec.js", "src/utils.spec.js", true),
            ("utils_test.go", "pkg/utils_test.go", true),
            ("FooTest.java", "src/FooTest.java", true),
            // Only the *Test.java suffix counts, not any file with "Test" in it.
            ("TestFoo.java", "src/TestFoo.java", false),
            ("helper.py", "tests/helper.py", true),
            ("helper.ts", "src/__tests__/helper.ts", true),
            ("helper.ts", "src/spec/helper.ts", true),
            // "/spec/" needs the trailing slash; "/specs/" is a different dir.
            ("helper.ts", "src/specs/helper.ts", false),
            // Sibling `tests.rs` (the `#[cfg(test)] mod tests;` split-file
            // convention used by large modules) and its `_tests.rs` variants.
            ("tests.rs", "src/output/parser/tests.rs", true),
            ("middleware_tests.rs", "src/server/auth/middleware_tests.rs", true),
            ("findings_tests.rs", "src/output/parser/findings_tests.rs", true),
            // `test.rs` (no `s`) is not the convention; `_test.rs` already matched.
            ("contest.rs", "src/contest.rs", false),
            // The `.test.` / `.spec.` / `_test.` infixes are extension-agnostic:
            // tsx/vue/etc. variants of the JS/TS conventions count too.
            ("app.test.tsx", "src/app.test.tsx", true),
            ("widget.spec.vue", "src/widget.spec.vue", true),
            ("parser_test.ts", "src/parser_test.ts", true),
            // "latest" contains "test" but not as a delimited infix.
            ("latest.rs", "src/latest.rs", false),
        ];
        for (name, path, expected) in cases {
            assert_eq!(is_test_file(name, path), *expected, "name={name} path={path}");
        }
    }

    #[test]
    fn is_ci_config_recognizes_well_known_ci_paths() {
        let cases: &[(&str, bool)] = &[
            (".gitlab-ci.yml", true),
            ("Jenkinsfile", true),
            ("azure-pipelines.yml", true),
            (".github/workflows/ci.yml", true),
            (".github/workflows/deploy.yaml", true),
            ("sub/.github/workflows/ci.yml", true),
            (".circleci/config.yml", true),
            ("sub/.circleci/config.yml", true),
            (".buildkite/pipeline.yml", true),
            ("src/main.rs", false),
            (".github/CODEOWNERS", false),
        ];
        for (path, expected) in cases {
            assert_eq!(is_ci_config(path), *expected, "path={path}");
        }
    }

    #[test]
    fn analyse_python_defs_counts_unterminated_signature_at_eof() {
        // Malformed input: the multi-line `def` never gets its trailing `:`,
        // but the heuristic still records it instead of dropping the count.
        let stats = analyse_python_defs("def f(\n    a: int\n    b: str");
        assert_eq!(stats.total, 1);
        assert_eq!(stats.return_annotated, 0);
        // No closing `)` means an empty (vacuously annotated) param list.
        assert_eq!(stats.fully_param_annotated, 1);
    }

    #[test]
    fn analyse_python_defs_caps_runaway_signatures_at_64_lines() {
        let mut content = String::from("def f(\n");
        for i in 0..100 {
            content.push_str(&format!("    p{i}: int,\n"));
        }
        let stats = analyse_python_defs(&content);
        // Exactly one def is recorded; the 64-line cap terminates the scan
        // instead of growing the accumulator forever.
        assert_eq!(stats.total, 1);
        assert_eq!(stats.return_annotated, 0);
    }

    #[test]
    fn record_signature_skips_star_and_slash_separator_markers() {
        // `*` and `/` are PEP 484/570 parameter separators, not real params;
        // both annotated params still make the signature fully annotated.
        let stats = analyse_python_defs("def f(*, a: int, /, b: int) -> None:\n    pass\n");
        assert_eq!(stats.total, 1);
        assert_eq!(stats.return_annotated, 1);
        assert_eq!(stats.fully_param_annotated, 1);
    }

    #[test]
    fn record_signature_varargs_count_as_unannotated() {
        // `*args` / `**kwargs` carry no annotation and must mark the
        // signature as NOT fully annotated.
        let stats = analyse_python_defs("def g(a: int, *args, **kwargs) -> None:\n    pass\n");
        assert_eq!(stats.total, 1);
        assert_eq!(stats.return_annotated, 1);
        assert_eq!(stats.fully_param_annotated, 0);
    }

    #[test]
    fn split_top_level_commas_respects_brackets_and_quotes() {
        let parts = split_top_level_commas("a: dict[str, int], b: str = \"x,y\", c: str = r\"p,q\"");
        // Whitespace around the separators is preserved; `record_signature`
        // trims each token before inspecting it.
        assert_eq!(
            parts,
            vec![
                "a: dict[str, int]".to_string(),
                " b: str = \"x,y\"".to_string(),
                " c: str = r\"p,q\"".to_string(),
            ]
        );
    }

    #[test]
    fn split_top_level_commas_ignores_escaped_quotes() {
        // A backslash-escaped quote inside a string must not terminate it.
        let parts = split_top_level_commas("x: str = \"a\\\"b\", y: int");
        assert_eq!(parts, vec!["x: str = \"a\\\"b\"".to_string(), " y: int".to_string()]);
    }
}
