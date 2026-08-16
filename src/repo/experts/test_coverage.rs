use anyhow::Result;
use async_trait::async_trait;

use super::facts::is_test_file;
use super::{ExpertScore, RepoContext, RepoExpert, ScoreItem};
use crate::llm::client::LLMClient;

pub struct TestCoverage;

#[async_trait]
impl RepoExpert for TestCoverage {
    fn name(&self) -> &str {
        "test_coverage"
    }
    fn weight(&self) -> u8 {
        20
    }
    fn requires_llm(&self) -> bool {
        false
    }

    async fn evaluate(&self, ctx: &RepoContext, _llm: Option<&LLMClient>) -> Result<ExpertScore> {
        let mut details = Vec::new();
        let all_files: Vec<_> = ctx.entries.iter().filter(|e| !e.is_binary && !e.is_generated).collect();
        let source_loc: usize = all_files.iter().map(|e| e.loc).sum();

        // Layer 1: test framework detection
        let has_dev_deps = ctx.entries.iter().any(|e| {
            if !e.path.ends_with("Cargo.toml")
                && !e.path.ends_with("pyproject.toml")
                && !e.path.ends_with("setup.py")
                && !e.path.ends_with("package.json")
            {
                return false;
            }
            if let Ok(c) = std::fs::read_to_string(&e.path) {
                if c.contains("[dev-dependencies]") {
                    return true;
                }
                if c.contains("[tool.pytest") {
                    return true;
                }
                if c.contains("devDependencies") {
                    return true;
                }
                if c.contains("pytest") || c.contains("unittest") {
                    return true;
                }
            }
            false
        });

        // Layer 2: test file detection (language-agnostic)
        let mut test_loc: usize = 0;
        let mut test_file_count: usize = 0;
        let mut has_inline_tests = false;

        for entry in all_files {
            let content = match std::fs::read_to_string(&entry.path) {
                Ok(c) => c,
                Err(_) => continue,
            };

            // Rust inline tests
            if entry.language == "Rust" && (content.contains("#[cfg(test)]") || content.contains("mod tests")) {
                has_inline_tests = true;
                let mut in_test = false;
                let mut brace_depth = 0i32;
                let mut test_lines = 0usize;
                for line in content.lines() {
                    let trimmed = line.trim();
                    if trimmed.starts_with("#[cfg(test)]") {
                        in_test = true;
                        continue;
                    }
                    if in_test && trimmed.contains('{') {
                        brace_depth += 1;
                    }
                    if in_test && trimmed.contains('}') {
                        brace_depth -= 1;
                    }
                    if in_test {
                        test_lines += 1;
                    }
                    if in_test && brace_depth <= 0 {
                        in_test = false;
                        brace_depth = 0;
                    }
                }
                test_loc += test_lines;
                test_file_count += 1;
                continue;
            }

            // Language-agnostic test file detection
            let name = std::path::Path::new(&entry.path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("");
            if is_test_file(name, &entry.path) {
                test_loc += entry.loc;
                test_file_count += 1;
            }
        }

        let test_ratio = if source_loc > 0 {
            test_loc as f64 / source_loc as f64
        } else {
            0.0
        };

        // Layer 3: CI test presence
        let has_ci_test = ctx.entries.iter().any(|e| is_ci_test_file(&e.path));

        let mut score: i32 = 0;
        if has_dev_deps {
            score += 10;
        }
        if has_inline_tests || test_file_count > 0 {
            score += 10;
        }
        if test_ratio > 0.3 {
            score += 50;
        } else if test_ratio > 0.1 {
            score += 30;
        } else if test_ratio > 0.0 {
            score += 15;
        }
        if has_ci_test {
            score += 30;
        }

        if test_file_count == 0 {
            if has_inline_tests {
                details.push(ScoreItem {
                    severity: "medium".to_string(),
                    message: format!(
                        "Tests are inline (in-source #[cfg(test)] blocks), no dedicated test files. Test ratio {:.1}%",
                        test_ratio * 100.0
                    ),
                    file: None,
                    ..Default::default()
                });
            } else {
                details.push(ScoreItem {
                    severity: "high".to_string(),
                    message: "No tests found in the repository".to_string(),
                    file: None,
                    ..Default::default()
                });
            }
        }
        if !has_dev_deps {
            details.push(ScoreItem {
                severity: "medium".to_string(),
                message: "No test framework configured".to_string(),
                file: None,
                ..Default::default()
            });
        }
        if !has_ci_test {
            details.push(ScoreItem {
                severity: "note".to_string(),
                message: "No CI test step detected".to_string(),
                file: None,
                ..Default::default()
            });
        }

        Ok(ExpertScore {
            expert_name: self.name().to_string(),
            weight: self.weight(),
            score: score.clamp(0, 100) as u8,
            summary: format!(
                "Test ratio {:.1}%, {} file(s) with tests, dev-deps={}, CI={}, inline={}",
                test_ratio * 100.0,
                test_file_count,
                if has_dev_deps { "yes" } else { "no" },
                if has_ci_test { "yes" } else { "no" },
                if has_inline_tests { "yes" } else { "no" },
            ),
            details,
        })
    }
}

/// Whether a file is a CI configuration that runs tests.
///
/// A file is a CI candidate when its path matches a well-known CI location
/// (`.gitlab-ci.yml`, `.github/workflows/`, `Jenkinsfile`), or when it is a
/// YAML file whose content mentions both "test" and "script". Either way the
/// content must mention "test". The file is read at most once.
fn is_ci_test_file(path: &str) -> bool {
    let content = std::fs::read_to_string(path).ok();
    // Accept both .gitlab-ci.yml and ./ci/some.yaml patterns
    let is_ci = path.contains(".gitlab-ci.yml")
        || path.contains(".github/workflows/")
        || path.contains("Jenkinsfile")
        || (path.ends_with(".yaml") || path.ends_with(".yml"))
            && content.as_deref().map_or(false, |c| c.contains("test"))
            && content.as_deref().map_or(false, |c| c.contains("script"));
    is_ci && content.as_deref().map_or(false, |c| c.contains("test"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_file(dir: &tempfile::TempDir, name: &str, content: &str) -> String {
        let path = dir.path().join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, content).unwrap();
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn yaml_with_test_and_script_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_file(&dir, "ci.yaml", "script:\n  - cargo test\n");
        assert!(is_ci_test_file(&path));
    }

    #[test]
    fn yaml_with_only_test_is_not_detected() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_file(&dir, "ci.yml", "stages:\n  - test\n");
        assert!(!is_ci_test_file(&path));
    }

    #[test]
    fn yaml_with_only_script_is_not_detected() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_file(&dir, "ci.yaml", "script:\n  - cargo build\n");
        assert!(!is_ci_test_file(&path));
    }

    #[test]
    fn known_ci_path_only_requires_test_in_content() {
        let dir = tempfile::tempdir().unwrap();
        // Path hit: the "script" requirement does not apply, "test" suffices.
        let path = write_file(&dir, ".gitlab-ci.yml", "stages:\n  - test\n");
        assert!(is_ci_test_file(&path));
        // But content without "test" is still rejected.
        let path = write_file(&dir, ".github/workflows/ci.yml", "on: push\njobs: {}\n");
        assert!(!is_ci_test_file(&path));
    }
}
