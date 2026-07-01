use anyhow::Result;
use async_trait::async_trait;

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

        // Test file naming conventions across languages:
        //   Rust:    *_test.rs, tests/*.rs
        //   Python:  test_*.py, *_test.py, tests/*.py
        //   JS/TS:   *.test.js, *.spec.js, __tests__/*.js
        //   Go:      *_test.go
        //   Java:    *Test.java, src/test/*
        let is_test_file = |name: &str, path: &str| -> bool {
            name.ends_with("_test.rs")
                || name.ends_with("_test.py")
                || name.starts_with("test_")
                || name.ends_with(".test.js")
                || name.ends_with(".spec.js")
                || name.ends_with(".test.ts")
                || name.ends_with(".spec.ts")
                || name.ends_with("_test.go")
                || name.ends_with("Test.java")
                || path.contains("/tests/")
                || path.contains("__tests__")
                || path.contains("/test/")
                || path.contains("/spec/")
        };

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
        let has_ci_test = ctx.entries.iter().any(|e| {
            let p = &e.path;
            // Accept both .gitlab-ci.yml and ./ci/some.yaml patterns
            let is_ci = p.contains(".gitlab-ci.yml")
                || p.contains(".github/workflows/")
                || p.contains("Jenkinsfile")
                || (p.ends_with(".yaml") || p.ends_with(".yml"))
                    && std::fs::read_to_string(p).ok().map_or(false, |c| c.contains("test"))
                    && std::fs::read_to_string(p).ok().map_or(false, |c| c.contains("script"));
            is_ci && std::fs::read_to_string(p).ok().map_or(false, |c| c.contains("test"))
        });

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
