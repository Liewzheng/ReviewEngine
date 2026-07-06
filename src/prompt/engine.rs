use anyhow::Result;
use minijinja::Environment;

use crate::models::*;
use crate::prompt::templates;

/// Template engine for building LLM prompts using the MiniJinja templating language.
///
/// Pre-loads all built-in prompt templates (review, aggregator, describe,
/// improve, ask, changelog, repo-review, overview) and renders them with
/// expert- and MR-specific context data.
pub struct PromptEngine {
    env: Environment<'static>,
}

impl PromptEngine {
    /// Create a new `PromptEngine` and register all built-in templates.
    ///
    /// Templates are loaded from `const` string constants embedded in
    /// the binary. Panics if any template fails to parse (this is
    /// considered a programming error).
    #[allow(clippy::unwrap_used)]
    pub fn new() -> Self {
        let mut env = Environment::new();
        env.add_template("review_system", templates::REVIEW_SYSTEM_TEMPLATE)
            .unwrap();
        env.add_template("review_user", templates::REVIEW_USER_TEMPLATE)
            .unwrap();
        env.add_template("aggregator_system", templates::AGGREGATOR_SYSTEM_TEMPLATE)
            .unwrap();
        env.add_template("aggregator_user", templates::AGGREGATOR_USER_TEMPLATE)
            .unwrap();
        env.add_template("describe_system", templates::DESCRIBE_SYSTEM_TEMPLATE)
            .unwrap();
        env.add_template("describe_user", templates::DESCRIBE_USER_TEMPLATE)
            .unwrap();
        env.add_template("improve_system", templates::IMPROVE_SYSTEM_TEMPLATE)
            .unwrap();
        env.add_template("improve_user", templates::IMPROVE_USER_TEMPLATE)
            .unwrap();
        env.add_template("ask_system", templates::ASK_SYSTEM_TEMPLATE).unwrap();
        env.add_template("ask_line_system", templates::ASK_LINE_SYSTEM_TEMPLATE)
            .unwrap();
        env.add_template("ask_user", templates::ASK_USER_TEMPLATE).unwrap();
        env.add_template("ask_line_user", templates::ASK_LINE_USER_TEMPLATE)
            .unwrap();
        env.add_template("repo_review_system", templates::REPO_REVIEW_SYSTEM_TEMPLATE)
            .unwrap();
        env.add_template("repo_review_user", templates::REPO_REVIEW_USER_TEMPLATE)
            .unwrap();
        env.add_template("changelog_system", templates::CHANGELOG_SYSTEM_TEMPLATE)
            .unwrap();
        env.add_template("changelog_user", templates::CHANGELOG_USER_TEMPLATE)
            .unwrap();
        env.add_template("overview_system", templates::OVERVIEW_SYSTEM_TEMPLATE)
            .unwrap();
        env.add_template("overview_user", templates::OVERVIEW_USER_TEMPLATE)
            .unwrap();
        Self { env }
    }

    /// Build a review system+user prompt pair for an individual expert.
    ///
    /// The system prompt includes the expert's perspective/role, the
    /// detected language, and max-findings limit. The user prompt
    /// contains the MR title, branch, description, optional lead context,
    /// and the full diff.
    ///
    /// Returns `(system_prompt, user_prompt)`.
    pub fn build_review_prompt(
        &self,
        expert: &ExpertDef,
        mr: &MRInfo,
        diff_text: &str,
        lang: &str,
        settings: &AppConfig,
        lead_context: Option<&GlobalReviewContext>,
    ) -> Result<(String, String)> {
        let ctx_system = serde_json::json!({
            "perspective": expert.prompt,
            "language": lang,
            "max_findings": settings.report.max_findings_per_expert,
        });

        let system = self.env.get_template("review_system")?.render(&ctx_system)?;

        let project = settings.project.as_ref();
        let project_type = project.and_then(|p| p.project_type.as_deref()).unwrap_or("");
        let os = project.and_then(|p| p.os.as_deref()).unwrap_or("");
        let arch = project.and_then(|p| p.arch.as_deref()).unwrap_or("");
        let domain = project.and_then(|p| p.domain.as_deref()).unwrap_or("");
        let constraints = project.and_then(|p| p.constraints.as_deref()).unwrap_or("");

        let lead_section = lead_context.map(|c| c.to_prompt_section());

        let ctx_user = serde_json::json!({
            "title": mr.title,
            "branch": mr.source_branch,
            "description": mr.description,
            "diff": diff_text,
            "project_type": project_type,
            "os": os,
            "arch": arch,
            "domain": domain,
            "constraints": constraints,
            "lead_context": lead_section,
        });

        let user = self.env.get_template("review_user")?.render(&ctx_user)?;

        Ok((system, user))
    }

    /// Build the aggregator prompt that consolidates multiple expert reports.
    ///
    /// Includes optional PR context (title, description, author, global
    /// context) and all individual expert reports. The system prompt
    /// instructs the LLM to merge, deduplicate, and sort findings.
    ///
    /// Returns `(system_prompt, user_prompt)`.
    pub fn build_aggregator_prompt(
        &self,
        reports: &[ExpertReport],
        mr_info: &MRInfo,
        global_context: Option<&GlobalReviewContext>,
        lang: &str,
    ) -> Result<(String, String)> {
        let ctx_system = serde_json::json!({});
        let system = self.env.get_template("aggregator_system")?.render(&ctx_system)?;

        let ctx_user = serde_json::json!({
            "reports": reports,
            "language": lang,
            "mr_title": mr_info.title,
            "mr_description": mr_info.description,
            "source_branch": mr_info.source_branch,
            "target_branch": mr_info.target_branch,
            "pr_author": mr_info.pr_author,
            "global_context": global_context,
            "has_pr_context": mr_info.mr_iid > 0,
        });

        let user = self.env.get_template("aggregator_user")?.render(&ctx_user)?;

        Ok((system, user))
    }

    /// Build a prompt that generates a PR description from the diff and commit messages.
    ///
    /// Returns `(system_prompt, user_prompt)` with the "describe" templates.
    pub fn build_describe_prompt(
        &self,
        diff: &str,
        mr: &MRInfo,
        commit_messages: &[String],
    ) -> Result<(String, String)> {
        let system = self
            .env
            .get_template("describe_system")?
            .render(&serde_json::json!({}))?;
        let ctx_user = serde_json::json!({
            "title": mr.title,
            "branch": mr.source_branch,
            "commit_messages": commit_messages,
            "diff": diff,
        });
        let user = self.env.get_template("describe_user")?.render(&ctx_user)?;
        Ok((system, user))
    }

    /// Build a prompt that suggests code improvements for the given diff.
    ///
    /// Returns `(system_prompt, user_prompt)` with the "improve" templates.
    pub fn build_improve_prompt(&self, diff: &str, mr: &MRInfo) -> Result<(String, String)> {
        let system = self
            .env
            .get_template("improve_system")?
            .render(&serde_json::json!({}))?;
        let ctx_user = serde_json::json!({
            "title": mr.title,
            "branch": mr.source_branch,
            "description": mr.description,
            "diff": diff,
        });
        let user = self.env.get_template("improve_user")?.render(&ctx_user)?;
        Ok((system, user))
    }

    /// Build a prompt that answers a free-form question about the diff.
    ///
    /// Returns `(system_prompt, user_prompt)` with the "ask" templates.
    pub fn build_ask_prompt(&self, question: &str, diff: &str, mr: &MRInfo) -> Result<(String, String)> {
        let system = self.env.get_template("ask_system")?.render(&serde_json::json!({}))?;
        let ctx_user = serde_json::json!({
            "title": mr.title,
            "branch": mr.source_branch,
            "question": question,
            "diff": diff,
        });
        let user = self.env.get_template("ask_user")?.render(&ctx_user)?;
        Ok((system, user))
    }

    /// Build a prompt that asks a question about a specific file and line.
    ///
    /// Includes the full file content and the question. The file extension
    /// is detected automatically for syntax-highlighting hints.
    ///
    /// Returns `(system_prompt, user_prompt)`.
    pub fn build_ask_line_prompt(
        &self,
        question: &str,
        file: &str,
        line: u32,
        file_content: &str,
    ) -> Result<(String, String)> {
        let system = self
            .env
            .get_template("ask_line_system")?
            .render(&serde_json::json!({}))?;
        let extension = std::path::Path::new(file)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        let ctx_user = serde_json::json!({
            "file": file,
            "line": line,
            "extension": extension,
            "file_content": file_content,
            "question": question,
        });
        let user = self.env.get_template("ask_line_user")?.render(&ctx_user)?;
        Ok((system, user))
    }

    /// Build a prompt for a full repository-level health review.
    ///
    /// Includes repository information, the file tree, and per-language
    /// statistics. The LLM produces a health score, risk map, and
    /// action items.
    ///
    /// Returns `(system_prompt, user_prompt)`.
    pub fn build_repo_review_prompt(
        &self,
        repo_info: &str,
        file_tree: &[String],
        language_stats: &std::collections::HashMap<String, u64>,
    ) -> Result<(String, String)> {
        let system = self
            .env
            .get_template("repo_review_system")?
            .render(&serde_json::json!({}))?;
        // Convert HashMap to Vec of objects for template iteration
        let lang_list: Vec<serde_json::Value> = language_stats
            .iter()
            .map(|(lang, loc)| serde_json::json!({"lang": lang, "loc": loc}))
            .collect();
        let ctx_user = serde_json::json!({
            "repo_info": repo_info,
            "file_tree": file_tree,
            "language_stats": lang_list,
        });
        let user = self.env.get_template("repo_review_user")?.render(&ctx_user)?;
        Ok((system, user))
    }

    /// Build a prompt that generates a CHANGELOG entry from the diff and commit messages.
    ///
    /// Returns `(system_prompt, user_prompt)` with the "changelog" templates.
    pub fn build_changelog_prompt(
        &self,
        diff: &str,
        commit_messages: &[String],
        mr: &MRInfo,
    ) -> Result<(String, String)> {
        let system = self
            .env
            .get_template("changelog_system")?
            .render(&serde_json::json!({}))?;
        let ctx_user = serde_json::json!({
            "title": mr.title,
            "branch": mr.source_branch,
            "commit_messages": commit_messages,
            "diff": diff,
        });
        let user = self.env.get_template("changelog_user")?.render(&ctx_user)?;
        Ok((system, user))
    }

    /// Build a prompt that produces a lead-reviewer overview of the PR.
    ///
    /// The LLM generates a branch summary (summary, risk areas, focus files,
    /// guidance) and a project overview from the project context.
    ///
    /// Returns `(system_prompt, user_prompt)`.
    pub fn build_overview_prompt(
        &self,
        mr: &MRInfo,
        project_config: Option<&crate::models::ProjectConfig>,
        project_context: &crate::context::ProjectContext,
        diff_text: &str,
    ) -> Result<(String, String)> {
        let system = self
            .env
            .get_template("overview_system")?
            .render(&serde_json::json!({}))?;

        let project_type = project_config.and_then(|p| p.project_type.as_deref()).unwrap_or("");
        let os = project_config.and_then(|p| p.os.as_deref()).unwrap_or("");
        let arch = project_config.and_then(|p| p.arch.as_deref()).unwrap_or("");
        let domain = project_config.and_then(|p| p.domain.as_deref()).unwrap_or("");
        let constraints = project_config.and_then(|p| p.constraints.as_deref()).unwrap_or("");

        let ctx_user = serde_json::json!({
            "title": mr.title,
            "branch": mr.source_branch,
            "description": mr.description,
            "diff": diff_text,
            "project_type": project_type,
            "os": os,
            "arch": arch,
            "domain": domain,
            "constraints": constraints,
            "project_context": project_context,
        });
        let user = self.env.get_template("overview_user")?.render(&ctx_user)?;
        Ok((system, user))
    }
}

impl Default for PromptEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_test_expert(prompt: &str) -> ExpertDef {
        ExpertDef {
            name: "test".to_string(),
            trigger: ExpertTrigger::Always,
            prompt: prompt.to_string(),
            config: ExpertTomlDef::default(),
        }
    }

    fn make_test_app_config(project: Option<ProjectConfig>) -> AppConfig {
        AppConfig {
            project,
            report: ReportConfig::default(),
            review_experts: HashMap::new(),
            commands: HashMap::new(),
            scoring: ScoringConfig::default(),
            llm: Vec::new(),
            output_dir: String::new(),
            max_team_size: None,
            max_concurrent_llm_calls: None,
            diff: DiffConfig::default(),
            rate_limit: RateLimitConfig::default(),
            languages: LanguagesConfig::default(),
        }
    }

    fn make_test_mr() -> MRInfo {
        MRInfo::new(
            "owner/repo".to_string(),
            "Add feature".to_string(),
            "feat/test".to_string(),
            "main".to_string(),
        )
    }

    #[test]
    fn test_review_prompt_with_project_context() {
        let engine = PromptEngine::new();
        let expert = make_test_expert("You are a security expert.");
        let project = ProjectConfig {
            name: Some("review-engine".to_string()),
            project_type: Some("embedded".to_string()),
            os: Some("Linux".to_string()),
            arch: Some("ARM".to_string()),
            domain: Some("IoT".to_string()),
            constraints: Some("single-threaded BLE stack, 64 KiB RAM".to_string()),
        };
        let settings = make_test_app_config(Some(project));
        let mr = make_test_mr();
        let (system, user) = engine
            .build_review_prompt(&expert, &mr, "diff", "zh", &settings, None)
            .unwrap();

        assert!(!system.is_empty());
        assert!(!user.is_empty());
        assert!(user.contains("## Project Context"));
        assert!(user.contains("Type: embedded"));
        assert!(user.contains("OS: Linux"));
        assert!(user.contains("Architecture: ARM"));
        assert!(user.contains("Domain: IoT"));
        assert!(user.contains("Constraints: single-threaded BLE stack, 64 KiB RAM"));
    }

    #[test]
    fn test_review_prompt_system_requires_structured_fields() {
        let engine = PromptEngine::new();
        let expert = make_test_expert("You are a performance expert.");
        let settings = make_test_app_config(None);
        let mr = make_test_mr();
        let (system, _user) = engine
            .build_review_prompt(&expert, &mr, "diff", "zh", &settings, None)
            .unwrap();

        assert!(system.contains("evidence"));
        assert!(system.contains("impact"));
        assert!(system.contains("recommendation"));
        assert!(system.contains("effort"));
        assert!(system.contains("line_end"));
        assert!(system.contains("Downgrade code-quality or style findings"));
    }

    #[test]
    fn test_review_prompt_without_project_context() {
        let engine = PromptEngine::new();
        let expert = make_test_expert("You are a lead reviewer.");
        let settings = make_test_app_config(None);
        let mr = make_test_mr();
        let (_system, user) = engine
            .build_review_prompt(&expert, &mr, "diff", "zh", &settings, None)
            .unwrap();

        assert!(!user.contains("## Project Context"));
    }

    #[test]
    fn test_review_prompt_with_lead_context() {
        let engine = PromptEngine::new();
        let expert = make_test_expert("You are a security expert.");
        let settings = make_test_app_config(None);
        let mr = make_test_mr();
        let lead = GlobalReviewContext {
            summary: "Add auth".to_string(),
            risk_areas: vec!["Security".to_string()],
            focus_files: vec!["src/auth.rs".to_string()],
            guidance: "Check token handling".to_string(),
            project_overview: "Rust web service".to_string(),
        };
        let (_system, user) = engine
            .build_review_prompt(&expert, &mr, "diff", "zh", &settings, Some(&lead))
            .unwrap();

        assert!(user.contains("## Lead Context"));
        assert!(user.contains("### Branch Summary"));
        assert!(user.contains("### Project Overview"));
        assert!(user.contains("Rust web service"));
    }

    #[test]
    fn test_review_prompt_with_lead_context_and_project_context() {
        let engine = PromptEngine::new();
        let expert = make_test_expert("You are a security expert.");
        let project = ProjectConfig {
            name: Some("review-engine".to_string()),
            project_type: Some("embedded".to_string()),
            os: Some("Linux".to_string()),
            arch: Some("ARM".to_string()),
            domain: Some("IoT".to_string()),
            constraints: Some("64 KiB RAM".to_string()),
        };
        let settings = make_test_app_config(Some(project));
        let mr = make_test_mr();
        let lead = GlobalReviewContext {
            summary: "Add auth".to_string(),
            risk_areas: vec!["Security".to_string()],
            focus_files: vec!["src/auth.rs".to_string()],
            guidance: "Check token handling".to_string(),
            project_overview: "Rust web service".to_string(),
        };
        let (_system, user) = engine
            .build_review_prompt(&expert, &mr, "diff", "zh", &settings, Some(&lead))
            .unwrap();

        assert!(user.contains("## Lead Context"));
        assert!(user.contains("## Project Context"));
        assert!(user.contains("Type: embedded"));
        assert!(user.contains("OS: Linux"));
        assert!(user.contains("Architecture: ARM"));
        assert!(user.contains("Domain: IoT"));
        assert!(user.contains("Constraints: 64 KiB RAM"));
    }

    #[test]
    fn test_overview_prompt_contains_project_context() {
        let engine = PromptEngine::new();
        let mr = make_test_mr();
        let project_config = ProjectConfig {
            name: Some("review-engine".to_string()),
            project_type: Some("Rust library".to_string()),
            os: None,
            arch: None,
            domain: None,
            constraints: None,
        };
        let project_context = crate::context::ProjectContext {
            readme_excerpt: "# Review Engine".to_string(),
            manifest_excerpt: "[package]\nname = \"review-engine\"\n".to_string(),
            file_tree: vec!["src/main.rs".to_string()],
            recent_commits: vec!["abc123 add feature".to_string()],
            branch_commits: vec!["def456 branch commit".to_string()],
        };
        let (system, user) = engine
            .build_overview_prompt(&mr, Some(&project_config), &project_context, "diff")
            .unwrap();

        assert!(system.contains("project_overview"));
        assert!(user.contains("# Review Engine"));
        assert!(user.contains("[package]"));
        assert!(user.contains("src/main.rs"));
        assert!(user.contains("abc123 add feature"));
        assert!(user.contains("def456 branch commit"));
        assert!(user.contains("Type: Rust library"));
    }

    #[test]
    fn test_aggregator_prompt_without_pr_context() {
        let engine = PromptEngine::new();
        let reports = vec![];
        let mr_info = MRInfo::new(
            "test".to_string(),
            "Local review".to_string(),
            "local".to_string(),
            "main".to_string(),
        );
        // mr_iid = 0 so has_pr_context = false, pr_author = None
        let (system, user) = engine.build_aggregator_prompt(&reports, &mr_info, None, "zh").unwrap();
        assert!(!system.is_empty());
        assert!(user.contains("## Expert Reports"));
        // No PR context block when has_pr_context is false
        assert!(!user.contains("## Pull Request Context"));
    }

    #[test]
    fn test_aggregator_prompt_with_pr_context() {
        let engine = PromptEngine::new();
        let reports = vec![];
        let mut mr_info = MRInfo::new(
            "owner/repo".to_string(),
            "Feat: add tests".to_string(),
            "feat/tests".to_string(),
            "main".to_string(),
        );
        mr_info.mr_iid = 42;
        mr_info.pr_author = Some("testuser".to_string());
        let (system, user) = engine.build_aggregator_prompt(&reports, &mr_info, None, "zh").unwrap();
        assert!(!system.is_empty());
        // PR context block should appear since has_pr_context = true
        assert!(user.contains("## Pull Request Context"));
        assert!(user.contains("Feat: add tests"));
        assert!(user.contains("testuser"));
    }

    #[test]
    fn test_aggregator_prompt_pr_author_none() {
        let engine = PromptEngine::new();
        let reports = vec![];
        let mut mr_info = MRInfo::new(
            "owner/repo".to_string(),
            "PR with deleted author".to_string(),
            "feature".to_string(),
            "main".to_string(),
        );
        mr_info.mr_iid = 99;
        // pr_author intentionally left as None
        let (system, user) = engine.build_aggregator_prompt(&reports, &mr_info, None, "zh").unwrap();
        assert!(!system.is_empty());
        assert!(user.contains("## Pull Request Context"));
        // pr_author None should render as empty string, not cause error
        assert!(user.contains("**Author**:"));
    }
}
