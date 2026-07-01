use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
/// Determines when an expert should participate in a review.
///
/// Each expert can define a trigger condition in the TOML config.
/// The trigger is evaluated at review time to decide whether to include the expert.
#[derive(Debug, Clone, JsonSchema)]
pub enum ExpertTrigger {
    /// Always include this expert in every review.
    Always,
    /// Only include when explicitly requested via `/review with <name>`.
    OnDemand,
    /// Include when the diff contains files matching the given glob patterns.
    FilePatterns { patterns: Vec<String> },
    /// Include when the diff involves files in the specified programming languages.
    Languages { languages: Vec<String> },
    /// Include when the diff has fewer than `max_files` changed files.
    DiffSize { max_files: usize },
}

impl ExpertTrigger {
    /// Convert the trigger into a human-readable perspective string for the LLM prompt.
    ///
    /// The perspective string tells the LLM what scope or focus this expert should have.
    pub fn to_perspective(&self) -> String {
        match self {
            ExpertTrigger::Always => String::new(),
            ExpertTrigger::OnDemand => "This expert is called on-demand for specific investigations.".to_string(),
            ExpertTrigger::FilePatterns { patterns } => {
                format!("Focus on files matching: {}", patterns.join(", "))
            }
            ExpertTrigger::Languages { languages } => {
                format!("Focus on languages: {}", languages.join(", "))
            }
            ExpertTrigger::DiffSize { max_files } => {
                format!("Focus on diffs with fewer than {} files.", max_files)
            }
        }
    }
}

impl<'de> serde::Deserialize<'de> for ExpertTrigger {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Helper enum to try string-first, then struct variants
        #[derive(serde::Deserialize)]
        #[serde(untagged)]
        enum TriggerRepr {
            Str(String),
            FileP { patterns: Vec<String> },
            Lang { languages: Vec<String> },
            DSize { max_files: usize },
        }

        match TriggerRepr::deserialize(deserializer)? {
            TriggerRepr::Str(s) => match s.to_lowercase().as_str() {
                "always" => Ok(ExpertTrigger::Always),
                "on_demand" => Ok(ExpertTrigger::OnDemand),
                _ => Err(serde::de::Error::unknown_variant(&s, &["always", "on_demand"])),
            },
            TriggerRepr::FileP { patterns } => Ok(ExpertTrigger::FilePatterns { patterns }),
            TriggerRepr::Lang { languages } => Ok(ExpertTrigger::Languages { languages }),
            TriggerRepr::DSize { max_files } => Ok(ExpertTrigger::DiffSize { max_files }),
        }
    }
}

impl serde::Serialize for ExpertTrigger {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            ExpertTrigger::Always => serializer.serialize_str("always"),
            ExpertTrigger::OnDemand => serializer.serialize_str("on_demand"),
            ExpertTrigger::FilePatterns { patterns } => {
                use serde::ser::SerializeMap;
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("patterns", patterns)?;
                map.end()
            }
            ExpertTrigger::Languages { languages } => {
                use serde::ser::SerializeMap;
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("languages", languages)?;
                map.end()
            }
            ExpertTrigger::DiffSize { max_files } => {
                use serde::ser::SerializeMap;
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("max_files", max_files)?;
                map.end()
            }
        }
    }
}

// ─── Expert TOML 定义 ────────────────────────

/// TOML-based expert definition as written in the config file.
///
/// Each entry under `[review_experts.<name>]` deserialises into this
/// struct. Fields map 1:1 to the TOML schema and are populated with
/// defaults when omitted. The runtime counterpart is [`ExpertDef`].
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExpertTomlDef {
    /// Whether this expert participates in reviews. Disabled experts are skipped.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// LLM model identifier to use for this expert (overrides the default).
    #[serde(default)]
    pub model: String,
    /// Human-readable title for the expert (e.g. "Security Lead").
    #[serde(default)]
    pub title: String,
    /// Role description passed to the LLM system prompt.
    #[serde(default)]
    pub role: String,
    /// Reviewing style / tone (e.g. "strict", "balanced", "supportive").
    #[serde(default)]
    pub style: String,
    /// Core principles this expert should uphold.
    #[serde(default)]
    pub principles: Vec<String>,
    /// Areas or aspects the expert should focus on.
    #[serde(default)]
    pub focus: Vec<String>,
    /// Coding standards and conventions the expert should enforce.
    #[serde(default)]
    pub standards: Vec<String>,
    /// Scoring weight (0–100). All enabled experts' weights must sum to 100.
    #[serde(default)]
    pub weight: u8,
    /// Commands this expert can handle (e.g. `["review", "describe"]`).
    #[serde(default)]
    pub commands: Vec<String>,
    /// Condition under which this expert is activated.
    pub trigger: Option<ExpertTrigger>,
    /// Expert's persona/prompt text injected into the LLM system prompt.
    /// If not set, a default perspective is generated from the trigger configuration.
    #[serde(alias = "trigger_prompt", alias = "perspective")]
    pub prompt: Option<String>,
}

impl ExpertTomlDef {
    /// Validate the expert definition.
    ///
    /// Returns an error if the expert is enabled but has an empty `role`.
    pub fn validate(&self, name: &str) -> anyhow::Result<()> {
        if self.enabled {
            if self.role.trim().is_empty() {
                anyhow::bail!("expert '{}' is enabled but has an empty 'role'", name);
            }
        }
        Ok(())
    }
}

fn default_enabled() -> bool {
    true
}

// ─── 运行时专家定义 ──────────────────────────

/// A fully resolved expert definition ready for use at review time.
///
/// Built from an [`ExpertTomlDef`] by resolving the trigger and prompt,
/// merging defaults, and stripping configuration overhead.
#[derive(Debug, Clone)]
pub struct ExpertDef {
    /// Unique name of the expert (matches the TOML key).
    pub name: String,
    /// Resolved trigger condition.
    pub trigger: ExpertTrigger,
    /// Prompt text injected into the LLM system prompt.
    pub prompt: String,
    /// Original TOML definition (carried for access to weight, style, etc.).
    pub config: ExpertTomlDef,
}

impl From<(&String, &ExpertTomlDef)> for ExpertDef {
    fn from((name, e): (&String, &ExpertTomlDef)) -> Self {
        let prompt = e
            .prompt
            .clone()
            .unwrap_or_else(|| e.trigger.as_ref().map(|t| t.to_perspective()).unwrap_or_default());
        ExpertDef {
            name: name.clone(),
            trigger: e.trigger.clone().unwrap_or(ExpertTrigger::Always),
            prompt,
            config: e.clone(),
        }
    }
}

// ─── 命令类型 ──────────────────────────────

/// An action command parsed from user or chat input.
///
/// Commands trigger different review workflows and are extensible
/// via the config file's `[commands]` section.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Command {
    /// Run a full code review on the MR/PR diff.
    Review,
    /// Generate a description / summary of the changes.
    Describe,
    /// Suggest specific code improvements.
    Improve,
    /// Ask a free-form question about the codebase (`ask <question>`).
    Ask(String),
    /// Run a full repository-level health assessment.
    RepoReview,
    /// Generate a changelog entry from the commits.
    UpdateChangelog,
    /// Show help / available commands.
    Help,
}

impl Command {
    /// Parse a command string into a [`Command`] variant.
    ///
    /// Matching is case-insensitive and tolerant of leading/trailing whitespace.
    /// Returns `None` for unrecognised inputs.
    pub fn parse(text: &str) -> Option<Command> {
        let text = text.trim().to_lowercase();
        match text.as_str() {
            "review" => Some(Command::Review),
            "describe" => Some(Command::Describe),
            "improve" => Some(Command::Improve),
            "ask" => Some(Command::Ask(String::new())),
            "repo_review" | "repo-review" => Some(Command::RepoReview),
            "update_changelog" | "update-changelog" => Some(Command::UpdateChangelog),
            "help" => Some(Command::Help),
            _ => {
                if let Some(q) = text.strip_prefix("ask ") {
                    Some(Command::Ask(q.trim().to_string()))
                } else {
                    None
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── Command::parse ──────────────────────────

    #[test]
    fn test_parse_review() {
        assert!(matches!(Command::parse("review"), Some(Command::Review)));
    }

    #[test]
    fn test_parse_describe() {
        assert!(matches!(Command::parse("describe"), Some(Command::Describe)));
    }

    #[test]
    fn test_parse_improve() {
        assert!(matches!(Command::parse("improve"), Some(Command::Improve)));
    }

    #[test]
    fn test_parse_ask() {
        assert!(matches!(Command::parse("ask"), Some(Command::Ask(s)) if s.is_empty()));
    }

    #[test]
    fn test_parse_ask_with_question() {
        assert!(matches!(Command::parse("ask why this fails"), Some(Command::Ask(s)) if s == "why this fails"));
    }

    #[test]
    fn test_parse_ask_prefix_edge_cases() {
        assert!(matches!(Command::parse("ask "), Some(Command::Ask(s)) if s.is_empty()));
        assert!(matches!(Command::parse("ask  why"), Some(Command::Ask(s)) if s == "why"));
        assert!(matches!(Command::parse("ASK question"), Some(Command::Ask(s)) if s == "question"));
    }

    #[test]
    fn test_parse_ask_whitespace_only() {
        assert!(matches!(Command::parse("ask   "), Some(Command::Ask(s)) if s.is_empty()));
    }

    #[test]
    fn test_parse_repo_review() {
        assert!(matches!(Command::parse("repo-review"), Some(Command::RepoReview)));
        assert!(matches!(Command::parse("repo_review"), Some(Command::RepoReview)));
    }

    #[test]
    fn test_parse_invalid() {
        assert!(Command::parse("").is_none());
        assert!(Command::parse("unknown").is_none());
    }

    #[test]
    fn test_parse_repo_review_invalid() {
        assert!(Command::parse("repo review").is_none());
    }

    #[test]
    fn test_parse_case_insensitive() {
        assert!(matches!(Command::parse("REVIEW"), Some(Command::Review)));
        assert!(matches!(Command::parse("ReViEw"), Some(Command::Review)));
        assert!(matches!(Command::parse("Describe"), Some(Command::Describe)));
        assert!(matches!(Command::parse("DESCRIBE"), Some(Command::Describe)));
        assert!(matches!(Command::parse("IMPROVE"), Some(Command::Improve)));
        assert!(matches!(Command::parse("AsK"), Some(Command::Ask(s)) if s.is_empty()));
        assert!(matches!(Command::parse("REPO-REVIEW"), Some(Command::RepoReview)));
        assert!(matches!(Command::parse("REPO_REVIEW"), Some(Command::RepoReview)));
    }

    #[test]
    fn test_parse_with_whitespace() {
        assert!(matches!(Command::parse(" review "), Some(Command::Review)));
        assert!(matches!(Command::parse("  describe  "), Some(Command::Describe)));
    }

    // ─── to_perspective ──────────────────────────

    #[test]
    fn test_trigger_to_perspective_always() {
        assert_eq!(ExpertTrigger::Always.to_perspective(), "");
    }

    #[test]
    fn test_trigger_to_perspective_on_demand() {
        let s = ExpertTrigger::OnDemand.to_perspective();
        assert!(s.contains("on-demand"));
    }

    #[test]
    fn test_trigger_to_perspective_file_patterns() {
        let t = ExpertTrigger::FilePatterns {
            patterns: vec!["**/*.rs".to_string(), "**/*.ts".to_string()],
        };
        let s = t.to_perspective();
        assert!(s.contains("**/*.rs"));
        assert!(s.contains("**/*.ts"));
    }

    #[test]
    fn test_trigger_to_perspective_languages() {
        let t = ExpertTrigger::Languages {
            languages: vec!["rust".to_string(), "python".to_string()],
        };
        let s = t.to_perspective();
        assert!(s.contains("rust"));
        assert!(s.contains("python"));
    }

    #[test]
    fn test_trigger_to_perspective_diff_size() {
        let t = ExpertTrigger::DiffSize { max_files: 10 };
        let s = t.to_perspective();
        assert!(s.contains("10"));
    }

    // ─── ExpertTrigger deserialization ──────────

    #[test]
    fn test_trigger_deserialize_always_from_string() {
        let toml_str = "trigger = \"always\"";
        #[derive(Deserialize)]
        struct Wrap {
            trigger: ExpertTrigger,
        }
        let w: Wrap = toml::from_str(toml_str).unwrap();
        assert!(matches!(w.trigger, ExpertTrigger::Always));
    }

    #[test]
    fn test_trigger_deserialize_on_demand_from_string() {
        let toml_str = "trigger = \"on_demand\"";
        #[derive(Deserialize)]
        struct Wrap {
            trigger: ExpertTrigger,
        }
        let w: Wrap = toml::from_str(toml_str).unwrap();
        assert!(matches!(w.trigger, ExpertTrigger::OnDemand));
    }

    #[test]
    fn test_trigger_deserialize_file_patterns() {
        let toml_str = r#"trigger = { patterns = ["*.rs", "*.ts"] }"#;
        #[derive(Deserialize)]
        struct Wrap {
            trigger: ExpertTrigger,
        }
        let w: Wrap = toml::from_str(toml_str).unwrap();
        assert!(
            matches!(&w.trigger, ExpertTrigger::FilePatterns { patterns } if patterns == &vec!["*.rs".to_string(), "*.ts".to_string()])
        );
    }

    #[test]
    fn test_trigger_deserialize_languages() {
        let toml_str = r#"trigger = { languages = ["rust", "python"] }"#;
        #[derive(Deserialize)]
        struct Wrap {
            trigger: ExpertTrigger,
        }
        let w: Wrap = toml::from_str(toml_str).unwrap();
        assert!(
            matches!(&w.trigger, ExpertTrigger::Languages { languages } if languages == &vec!["rust".to_string(), "python".to_string()])
        );
    }

    #[test]
    fn test_trigger_deserialize_diff_size() {
        let toml_str = r#"trigger = { max_files = 10 }"#;
        #[derive(Deserialize)]
        struct Wrap {
            trigger: ExpertTrigger,
        }
        let w: Wrap = toml::from_str(toml_str).unwrap();
        assert!(matches!(w.trigger, ExpertTrigger::DiffSize { max_files } if max_files == 10));
    }

    #[test]
    fn test_trigger_deserialize_invalid_string_fails() {
        let toml_str = "trigger = \"bogus\"";
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct Wrap {
            trigger: ExpertTrigger,
        }
        let result = toml::from_str::<Wrap>(toml_str);
        assert!(result.is_err());
    }

    // ─── ExpertDef::from ─────────────────────────

    #[test]
    fn test_expert_def_from_uses_prompt_when_set() {
        let mut toml = ExpertTomlDef::default();
        toml.prompt = Some("Custom prompt".to_string());
        let def = ExpertDef::from((&"test".to_string(), &toml));
        assert_eq!(def.prompt, "Custom prompt");
    }

    #[test]
    fn test_expert_def_from_falls_back_to_trigger_perspective() {
        let mut toml = ExpertTomlDef::default();
        toml.prompt = None;
        toml.trigger = Some(ExpertTrigger::OnDemand);
        let def = ExpertDef::from((&"test".to_string(), &toml));
        // When no prompt set, should use trigger.to_perspective()
        assert_eq!(def.prompt, ExpertTrigger::OnDemand.to_perspective());
    }

    #[test]
    fn test_expert_def_from_default_trigger_when_none() {
        let toml = ExpertTomlDef::default();
        let def = ExpertDef::from((&"test".to_string(), &toml));
        assert!(matches!(def.trigger, ExpertTrigger::Always));
        assert_eq!(def.prompt, "");
    }

    // ─── ExpertTomlDef::validate ─────────────────

    #[test]
    fn test_expert_toml_validate_empty_role_enabled() {
        let expert = ExpertTomlDef {
            enabled: true,
            role: String::new(),
            ..Default::default()
        };
        assert!(expert.validate("test").is_err());
    }

    #[test]
    fn test_expert_toml_validate_empty_role_disabled() {
        let expert = ExpertTomlDef {
            enabled: false,
            role: String::new(),
            ..Default::default()
        };
        assert!(expert.validate("test").is_ok());
    }

    #[test]
    fn test_expert_toml_validate_non_empty_role() {
        let expert = ExpertTomlDef {
            enabled: true,
            role: "Security Lead".to_string(),
            ..Default::default()
        };
        assert!(expert.validate("test").is_ok());
    }
}
