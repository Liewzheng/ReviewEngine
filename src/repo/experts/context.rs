use crate::repo::FileEntry;
use crate::repo::RepoStats;

/// Global context produced by the Architecture Lead in Pass 1.
/// Injected into all Pass 2 LLM expert prompts.
#[derive(Debug, Clone)]
pub struct RepoGlobalContext {
    pub summary: String,
    pub risk_areas: Vec<String>,
    pub focus_modules: Vec<String>,
    pub expert_guidance: String,
}

impl RepoGlobalContext {
    /// Render the context as a markdown section for LLM prompts.
    pub fn to_prompt_section(&self) -> String {
        let mut s = String::from("## Repository Context (AI-Vetted)\n\n");

        s.push_str(&format!("**Summary**: {}\n\n", self.summary));

        if !self.risk_areas.is_empty() {
            s.push_str("**Risk Areas**:\n");
            for area in &self.risk_areas {
                s.push_str(&format!("- {}\n", area));
            }
            s.push('\n');
        }

        if !self.focus_modules.is_empty() {
            s.push_str("**Focus Modules**:\n");
            for m in &self.focus_modules {
                s.push_str(&format!("- {}\n", m));
            }
            s.push('\n');
        }

        if !self.expert_guidance.is_empty() {
            s.push_str(&format!("**Expert Guidance**: {}\n\n", self.expert_guidance));
        }

        s
    }

    /// Build a minimal context from file tree statistics (fallback when no LLM).
    pub fn from_stats(entries: &[FileEntry], stats: &RepoStats) -> Self {
        let summary = format!(
            "Repository has {} files ({} LOC). {} languages detected. {} source files.",
            stats.total_files,
            stats.total_loc,
            stats.languages.len(),
            entries.iter().filter(|e| !e.is_binary && !e.is_generated).count(),
        );

        let modules: Vec<String> = {
            let mut dirs: Vec<String> = entries
                .iter()
                .filter_map(|e| {
                    let path = std::path::Path::new(&e.path);
                    path.parent().and_then(|p| p.to_str()).map(|p| p.to_string())
                })
                .collect();
            dirs.sort();
            dirs.dedup();
            dirs.into_iter().filter(|d| d.starts_with("src/")).take(10).collect()
        };

        RepoGlobalContext {
            summary,
            risk_areas: Vec::new(),
            focus_modules: modules,
            expert_guidance: String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::{FileEntry, LanguageStats};
    use std::collections::HashMap;

    fn make_entry(path: &str, language: &str, loc: usize) -> FileEntry {
        FileEntry {
            path: path.to_string(),
            language: language.to_string(),
            loc,
            is_binary: false,
            is_generated: false,
        }
    }

    fn make_stats(total_files: usize, total_loc: usize, languages: Vec<(&str, usize, usize)>) -> RepoStats {
        let mut lang_map = HashMap::new();
        for (name, files, loc) in languages {
            lang_map.insert(name.to_string(), LanguageStats { files, loc });
        }
        RepoStats {
            total_files,
            total_loc,
            languages: lang_map,
            large_files: vec![],
            generated_files: 0,
            binary_files: 0,
        }
    }

    // ─── from_stats ───────────────────────────

    #[test]
    fn test_from_stats_empty_repo() {
        let ctx = RepoGlobalContext::from_stats(&[], &RepoStats::default());
        assert!(ctx.summary.contains("0 files"));
        assert!(ctx.summary.contains("0 LOC"));
        assert!(ctx.focus_modules.is_empty());
        assert!(ctx.risk_areas.is_empty());
        assert!(ctx.expert_guidance.is_empty());
    }

    #[test]
    fn test_from_stats_single_language() {
        let entries = vec![make_entry("src/main.rs", "Rust", 50)];
        let stats = make_stats(1, 50, vec![("Rust", 1, 50)]);
        let ctx = RepoGlobalContext::from_stats(&entries, &stats);
        assert!(ctx.summary.contains("1 files"));
        assert!(ctx.summary.contains("50 LOC"));
        assert!(ctx.summary.contains("1 languages"));
    }

    #[test]
    fn test_from_stats_multiple_languages() {
        let entries = vec![
            make_entry("src/main.rs", "Rust", 100),
            make_entry("src/lib.py", "Python", 200),
            make_entry("README.md", "Markdown", 10),
        ];
        let stats = make_stats(3, 310, vec![("Rust", 1, 100), ("Python", 1, 200), ("Markdown", 1, 10)]);
        let ctx = RepoGlobalContext::from_stats(&entries, &stats);
        assert!(ctx.summary.contains("3 files"));
        assert!(ctx.summary.contains("310 LOC"));
        assert!(ctx.summary.contains("3 languages"));
    }

    #[test]
    fn test_from_stats_focus_modules() {
        let entries = vec![
            make_entry("src/main.rs", "Rust", 50),
            make_entry("src/parser/mod.rs", "Rust", 100),
            make_entry("src/config.rs", "Rust", 30),
            make_entry("tests/test_main.rs", "Rust", 20),
            make_entry("README.md", "Markdown", 5),
        ];
        let stats = make_stats(5, 205, vec![("Rust", 4, 200), ("Markdown", 1, 5)]);
        let ctx = RepoGlobalContext::from_stats(&entries, &stats);
        // Should only include dirs starting with "src/"
        assert!(ctx.focus_modules.iter().any(|m| m == "src/parser" || m == "src"));
        assert!(!ctx.focus_modules.iter().any(|m| m == "tests"));
    }

    #[test]
    fn test_from_stats_limits_focus_modules() {
        let mut entries = Vec::new();
        for i in 0..15 {
            entries.push(make_entry(&format!("src/mod{}/file.rs", i), "Rust", 10));
        }
        let stats = make_stats(15, 150, vec![("Rust", 15, 150)]);
        let ctx = RepoGlobalContext::from_stats(&entries, &stats);
        assert!(ctx.focus_modules.len() <= 10);
    }

    // ─── to_prompt_section ────────────────────

    #[test]
    fn test_to_prompt_section_empty() {
        let ctx = RepoGlobalContext {
            summary: String::new(),
            risk_areas: vec![],
            focus_modules: vec![],
            expert_guidance: String::new(),
        };
        let s = ctx.to_prompt_section();
        assert!(s.contains("Repository Context"));
        assert!(s.contains("**Summary**:"));
        assert!(!s.contains("Risk Areas"));
        assert!(!s.contains("Focus Modules"));
        assert!(!s.contains("Expert Guidance"));
    }

    #[test]
    fn test_to_prompt_section_with_all_fields() {
        let ctx = RepoGlobalContext {
            summary: "Test repository".to_string(),
            risk_areas: vec!["Security concern".to_string(), "Performance issue".to_string()],
            focus_modules: vec!["src/core".to_string()],
            expert_guidance: "Focus on error handling".to_string(),
        };
        let s = ctx.to_prompt_section();
        assert!(s.contains("**Summary**: Test repository"));
        assert!(s.contains("**Risk Areas**"));
        assert!(s.contains("- Security concern"));
        assert!(s.contains("- Performance issue"));
        assert!(s.contains("**Focus Modules**"));
        assert!(s.contains("- src/core"));
        assert!(s.contains("**Expert Guidance**: Focus on error handling"));
    }

    #[test]
    fn test_to_prompt_section_with_risk_areas_only() {
        let ctx = RepoGlobalContext {
            summary: "Has risks".to_string(),
            risk_areas: vec!["Risk 1".to_string()],
            focus_modules: vec![],
            expert_guidance: String::new(),
        };
        let s = ctx.to_prompt_section();
        assert!(s.contains("Risk Areas"));
        assert!(s.contains("- Risk 1"));
        assert!(!s.contains("Focus Modules"));
        assert!(!s.contains("Expert Guidance"));
    }

    #[test]
    fn test_to_prompt_section_with_focus_modules_only() {
        let ctx = RepoGlobalContext {
            summary: "Modules".to_string(),
            risk_areas: vec![],
            focus_modules: vec!["src/core".to_string(), "src/api".to_string()],
            expert_guidance: String::new(),
        };
        let s = ctx.to_prompt_section();
        assert!(s.contains("Focus Modules"));
        assert!(s.contains("- src/core"));
        assert!(s.contains("- src/api"));
        assert!(!s.contains("Risk Areas"));
        assert!(!s.contains("Expert Guidance"));
    }

    #[test]
    fn test_to_prompt_section_with_guidance_only() {
        let ctx = RepoGlobalContext {
            summary: "Guide".to_string(),
            risk_areas: vec![],
            focus_modules: vec![],
            expert_guidance: "Check error paths".to_string(),
        };
        let s = ctx.to_prompt_section();
        assert!(s.contains("Expert Guidance"));
        assert!(s.contains("Check error paths"));
        assert!(!s.contains("Risk Areas"));
        assert!(!s.contains("Focus Modules"));
    }

    #[test]
    fn test_from_stats_with_binary_and_generated_files() {
        let entries = vec![
            make_entry("src/main.rs", "Rust", 100),
            FileEntry {
                path: "image.png".to_string(),
                language: "Other".to_string(),
                loc: 0,
                is_binary: true,
                is_generated: false,
            },
            FileEntry {
                path: "generated/code.rs".to_string(),
                language: "Rust".to_string(),
                loc: 500,
                is_binary: false,
                is_generated: true,
            },
        ];
        let stats = make_stats(3, 600, vec![("Rust", 2, 600), ("Other", 1, 0)]);
        let ctx = RepoGlobalContext::from_stats(&entries, &stats);
        // Summary should mention total files (3) and source files (1 non-binary, non-generated)
        assert!(ctx.summary.contains("3 files"));
        assert!(ctx.summary.contains("600 LOC"));
    }

    #[test]
    fn test_from_stats_no_src_directory() {
        let entries = vec![
            make_entry("README.md", "Markdown", 10),
            make_entry("Makefile", "Other", 20),
        ];
        let stats = make_stats(2, 30, vec![("Markdown", 1, 10), ("Other", 1, 20)]);
        let ctx = RepoGlobalContext::from_stats(&entries, &stats);
        // No src/ dirs, so focus_modules should be empty
        assert!(ctx.focus_modules.is_empty());
    }

    #[test]
    fn test_repo_global_context_debug_and_clone() {
        let ctx = RepoGlobalContext {
            summary: "test".to_string(),
            risk_areas: vec!["r1".to_string()],
            focus_modules: vec!["m1".to_string()],
            expert_guidance: "g1".to_string(),
        };
        // Verify Debug and Clone traits are available
        let _formatted = format!("{:?}", ctx);
        let cloned = ctx.clone();
        assert_eq!(cloned.summary, "test");
        assert_eq!(cloned.risk_areas.len(), 1);
        assert_eq!(cloned.focus_modules.len(), 1);
        assert_eq!(cloned.expert_guidance, "g1");
    }
}
