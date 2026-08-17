    use super::scoring::*;
use super::architecture::ArchitectureLead;
use super::code_quality::CodeQuality;
use crate::repo::experts::{ExpertScore, RepoContext, RepoExpert};
use crate::llm::client::LLMClient;
use super::*;
    use crate::repo::experts::ScoreItem;

    // ─── YAML parsing fallback patterns ──────────
    // These test the same serde_yaml_ng::Value accessor chains used by
    // ArchitectureLead::evaluate and CodeQuality::evaluate.

    fn parse_score(yaml: &str) -> u8 {
        let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(yaml).unwrap_or(serde_yaml_ng::Value::Null);
        value["score"].as_u64().unwrap_or(70).min(100) as u8
    }

    fn parse_summary(yaml: &str, fallback: &str) -> String {
        let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(yaml).unwrap_or(serde_yaml_ng::Value::Null);
        value["summary"].as_str().unwrap_or(fallback).to_string()
    }

    fn parse_risk_areas(yaml: &str) -> Vec<String> {
        let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(yaml).unwrap_or(serde_yaml_ng::Value::Null);
        value["risk_areas"]
            .as_sequence()
            .map(|seq| seq.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default()
    }

    fn parse_findings(yaml: &str) -> Vec<ScoreItem> {
        let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(yaml).unwrap_or(serde_yaml_ng::Value::Null);
        let mut details = Vec::new();
        if let Some(findings) = value["findings"].as_sequence() {
            for f in findings {
                details.push(ScoreItem {
                    severity: f["severity"].as_str().unwrap_or("medium").to_string(),
                    message: f["message"].as_str().unwrap_or("").to_string(),
                    file: f["file"].as_str().map(String::from),
                    ..Default::default()
                });
            }
        }
        details
    }

    #[test]
    fn test_yaml_score_parsed() {
        assert_eq!(parse_score("score: 85"), 85);
    }

    #[test]
    fn test_yaml_score_missing_fallback() {
        assert_eq!(parse_score("summary: \"No score\""), 70);
    }

    #[test]
    fn test_yaml_score_clamped_max() {
        assert_eq!(parse_score("score: 150"), 100);
    }

    #[test]
    fn test_yaml_score_zero() {
        assert_eq!(parse_score("score: 0"), 0);
    }

    #[test]
    fn test_yaml_score_non_numeric() {
        assert_eq!(parse_score("score: \"abc\""), 70);
    }

    #[test]
    fn test_yaml_summary_parsed() {
        assert_eq!(
            parse_summary("summary: \"Custom arch\"", "Architecture assessment completed"),
            "Custom arch"
        );
    }

    #[test]
    fn test_yaml_summary_missing_arch_fallback() {
        assert_eq!(
            parse_summary("score: 80", "Architecture assessment completed"),
            "Architecture assessment completed"
        );
    }

    #[test]
    fn test_yaml_summary_missing_quality_fallback() {
        assert_eq!(
            parse_summary("score: 80", "Code quality assessment completed"),
            "Code quality assessment completed"
        );
    }

    #[test]
    fn test_yaml_risk_areas_parsed() {
        let areas = parse_risk_areas("risk_areas:\n  - \"Tight coupling\"\n  - \"Missing errors\"");
        assert_eq!(areas.len(), 2);
        assert!(areas[0].contains("Tight coupling"));
    }

    #[test]
    fn test_yaml_risk_areas_missing() {
        let areas = parse_risk_areas("score: 90");
        assert!(areas.is_empty());
    }

    #[test]
    fn test_yaml_guidance_fallback() {
        let yaml = "score: 80";
        let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(yaml).unwrap_or(serde_yaml_ng::Value::Null);
        let guidance = value["guidance"].as_str().unwrap_or("").to_string();
        assert_eq!(guidance, "");
    }

    #[test]
    fn test_yaml_findings_parsed() {
        let yaml = r#"
findings:
  - severity: "high"
    message: "Unsafe code"
    file: "src/main.rs"
  - severity: "low"
    message: "Missing docs"
"#;
        let details = parse_findings(yaml);
        assert_eq!(details.len(), 2);
        assert_eq!(details[0].severity, "high");
        assert_eq!(details[0].file.as_deref(), Some("src/main.rs"));
        assert_eq!(details[1].file, None);
    }

    #[test]
    fn test_yaml_findings_missing() {
        let details = parse_findings("score: 95");
        assert!(details.is_empty());
    }

    #[test]
    fn test_yaml_findings_missing_fields() {
        let yaml = "findings:\n  - severity: \"high\"\n";
        let details = parse_findings(yaml);
        assert_eq!(details.len(), 1);
        assert_eq!(details[0].severity, "high");
        assert_eq!(details[0].message, "");
    }

    #[test]
    fn test_yaml_null_value() {
        let value = serde_yaml_ng::Value::Null;
        assert_eq!(value["score"].as_u64().unwrap_or(70).min(100) as u8, 70);
        assert_eq!(
            value["summary"].as_str().unwrap_or("Architecture assessment completed"),
            "Architecture assessment completed"
        );
    }

    #[test]
    fn test_yaml_empty_document() {
        assert_eq!(parse_score(""), 70);
    }

    // ─── parse_expert_yaml fallback / robustness ──────────
    // These exercise the shared entry point used by both LLM experts: on any
    // failure it returns `Null` (so score falls back to 70 and findings to
    // empty) and logs a distinct warn — never panics.

    #[test]
    fn test_parse_expert_yaml_plain_yaml() {
        let v = parse_expert_yaml("code_quality", "score: 85\nsummary: \"ok\"", CODE_QUALITY_KEYS);
        assert_eq!(v["score"].as_u64(), Some(85));
    }

    #[test]
    fn test_parse_expert_yaml_fenced_yaml_recovers() {
        let raw = "Here is my assessment:\n```yaml\nscore: 88\nfindings: []\n```\n";
        let v = parse_expert_yaml("code_quality", raw, CODE_QUALITY_KEYS);
        assert_eq!(v["score"].as_u64(), Some(88));
    }

    #[test]
    fn test_parse_expert_yaml_architecture_keys_accepted() {
        let raw = "summary: \"ok\"\nscore: 72\nrisk_areas: []\n";
        let v = parse_expert_yaml("architecture_lead", raw, ARCHITECTURE_LEAD_KEYS);
        assert_eq!(v["score"].as_u64(), Some(72));
    }

    #[test]
    fn test_parse_expert_yaml_empty_response_falls_back() {
        // Observed failure mode: reasoning models exhaust their max_tokens
        // budget and return zero bytes. Must fall back, not panic.
        let v = parse_expert_yaml("code_quality", "", CODE_QUALITY_KEYS);
        assert!(v.is_null());
        let score = v["score"].as_u64().unwrap_or(70).min(100) as u8;
        assert_eq!(score, 70);
    }

    #[test]
    fn test_parse_expert_yaml_malformed_yaml_falls_back_without_panic() {
        let raw = "score: [unclosed\n  findings:\n    - severity: \"high\"\n";
        let v = parse_expert_yaml("code_quality", raw, CODE_QUALITY_KEYS);
        assert!(v.is_null());
        // Fallback score and empty findings, mirroring the evaluate path.
        let score = v["score"].as_u64().unwrap_or(70).min(100) as u8;
        assert_eq!(score, 70);
        let details = if let Some(findings) = v["findings"].as_sequence() {
            crate::repo::experts::parse_yaml_findings(findings)
        } else {
            Vec::new()
        };
        assert!(details.is_empty());
    }

    #[test]
    fn test_parse_expert_yaml_schema_drift_falls_back() {
        // Model returned valid YAML with the wrong shape (no expected keys):
        // must fall back rather than silently report 70/0 as model output.
        let raw = "verdicts: []\n";
        let v = parse_expert_yaml("code_quality", raw, CODE_QUALITY_KEYS);
        assert!(v.is_null());
    }

    #[test]
    fn test_parse_expert_yaml_prose_without_fence_falls_back() {
        let raw = "The module looks fine overall. No issues worth flagging.";
        let v = parse_expert_yaml("code_quality", raw, CODE_QUALITY_KEYS);
        assert!(v.is_null());
    }

    #[test]
    fn test_truncate_excerpt_bounds_length_and_collapses_newlines() {
        let long = "x".repeat(500);
        let ex = truncate_excerpt(&long);
        assert!(ex.len() <= EXCERPT_MAX_BYTES);
        assert!(!ex.is_empty());
        assert!(!ex.contains('\n'));
        assert_eq!(truncate_excerpt("line1\nline2"), "line1\\nline2");
        assert_eq!(truncate_excerpt(""), "");
    }

    #[test]
    fn test_render_code_quality_system_substitutes_placeholders() {
        let rendered = render_code_quality_system("auth", "Rust", "use snake_case names", "prefer Result");
        assert!(rendered.contains("**auth**"));
        assert!(rendered.contains("Primary language: Rust"));
        assert!(rendered.contains("use snake_case names"));
        assert!(rendered.contains("prefer Result"));
    }

    #[test]
    fn test_render_code_quality_system_leaves_no_placeholder_residue() {
        // Regression guard: every `{{ ... }}` marker in the template must be
        // substituted — a literal marker reaching the LLM means the replace
        // targets drifted from the template again.
        let rendered = render_code_quality_system("m", "l", "n", "e");
        assert!(
            !rendered.contains("{{"),
            "unsubstituted placeholder in prompt:\n{rendered}"
        );
        assert!(
            !rendered.contains("}}"),
            "unsubstituted placeholder in prompt:\n{rendered}"
        );
    }

    #[test]
    fn test_architecture_lead_metadata() {
        let expert = ArchitectureLead;
        assert_eq!(expert.weight(), 15);
        // Canonical area name: `convert_scores` keys the lead summary off
        // "architecture" and `DEFAULT_WEIGHTS` lists the same name.
        assert_eq!(expert.name(), "architecture");
        assert!(expert.requires_llm());
    }

    #[test]
    fn test_code_quality_metadata() {
        let expert = CodeQuality;
        assert_eq!(expert.weight(), 10);
        assert_eq!(expert.name(), "code_quality");
        assert!(expert.requires_llm());
    }

    // ─── facts-block injection ─────

    fn prompt_ctx(facts_block: Option<String>) -> RepoContext {
        RepoContext {
            entries: vec![],
            stats: crate::repo::RepoStats::default(),
            llm_configs: vec![],
            config: None,
            facts_block,
        }
    }

    #[test]
    fn test_architecture_prompt_injects_facts_block() {
        let ctx = prompt_ctx(Some("repo_facts:\n  test_files: 3\n".to_string()));
        let prompt = architecture_user_prompt(&ctx);
        assert!(prompt.contains("## Repository Facts (deterministic static analysis)"));
        assert!(prompt.contains("repo_facts:"));
        assert!(prompt.contains("test_files: 3"));
    }

    #[test]
    fn test_code_quality_prompt_injects_facts_block() {
        let ctx = prompt_ctx(Some(
            "repo_facts:\n  ci_configs:\n    - \".gitlab-ci.yml\"\n".to_string(),
        ));
        let prompt = code_quality_user_prompt(&ctx, "auth", "Rust", &["// code".to_string()]);
        assert!(prompt.contains("## Repository Facts (deterministic static analysis)"));
        assert!(prompt.contains("repo_facts:"));
        assert!(prompt.contains(".gitlab-ci.yml"));
    }

    #[test]
    fn test_prompts_omit_facts_block_when_absent() {
        // Local-only path (`facts_block: None`): no residue in either prompt.
        let ctx = prompt_ctx(None);
        assert!(!architecture_user_prompt(&ctx).contains("repo_facts"));
        assert!(!code_quality_user_prompt(&ctx, "m", "Rust", &[]).contains("repo_facts"));
    }

    #[test]
    fn test_fully_annotated_python_facts_reach_prompt_verbatim() {
        // The anti-"Missing type hints" chain: static full-annotation
        // coverage of 1.00 must be visible verbatim in the scored prompt.
        let dir = tempfile::tempdir().expect("tempdir");
        let py = dir.path().join("a.py");
        std::fs::write(&py, "def f(x: int) -> int:\n    return x\n").expect("write fixture");
        let entries = vec![crate::repo::FileEntry {
            path: py.to_string_lossy().into_owned(),
            language: "Python".to_string(),
            loc: 2,
            is_binary: false,
            is_generated: false,
        }];
        let block = crate::repo::experts::facts::compute(&entries).to_prompt_block();
        assert!(block.contains("full_param_annotation_coverage: 1.00"));

        let ctx = prompt_ctx(Some(block));
        let prompt = code_quality_user_prompt(&ctx, "m", "Python", &[]);
        assert!(prompt.contains("full_param_annotation_coverage: 1.00"));
        let arch_prompt = architecture_user_prompt(&ctx);
        assert!(arch_prompt.contains("full_param_annotation_coverage: 1.00"));
    }

    // ─── score sampling ─────

    #[test]
    fn test_median_score_odd_even_empty() {
        assert_eq!(median_score(&[]), None);
        assert_eq!(median_score(&[80]), Some(80));
        assert_eq!(median_score(&[70, 90, 80]), Some(80));
        // Even count: round-half-up mean of the two middle scores.
        assert_eq!(median_score(&[70, 80, 80, 91]), Some(80));
        assert_eq!(median_score(&[70, 71]), Some(71)); // 70.5 rounds up
        assert_eq!(median_score(&[70, 72]), Some(71));
        // Input order does not matter.
        assert_eq!(median_score(&[95, 40, 60]), Some(60));
    }

    #[test]
    fn test_scoring_sample_count_defaults_and_guards() {
        assert_eq!(scoring_sample_count(None), 1);
        let config: crate::models::AppConfig = toml::from_str("").unwrap();
        assert_eq!(scoring_sample_count(Some(&config)), 1);
        // 0 is meaningless: treated as the single-call status quo.
        let config: crate::models::AppConfig = toml::from_str("[scoring]\nscore_samples = 0\n").unwrap();
        assert_eq!(scoring_sample_count(Some(&config)), 1);
        let config: crate::models::AppConfig = toml::from_str("[scoring]\nscore_samples = 5\n").unwrap();
        assert_eq!(scoring_sample_count(Some(&config)), 5);
    }

    use crate::llm::provider::{CompletionParams, CompletionResult, LLMProvider, ProviderRegistry};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// Mock provider serving scripted response bodies in poll order (the
    /// last body repeats). Tracks peak in-flight concurrency so tests can
    /// prove sampling is concurrent, and records the temperature of every
    /// request so tests can prove the scoring override reaches the wire.
    struct ScriptedProvider {
        bodies: Vec<String>,
        calls: AtomicUsize,
        in_flight: Arc<AtomicUsize>,
        peak: Arc<AtomicUsize>,
        temperatures: Arc<std::sync::Mutex<Vec<f32>>>,
    }

    #[async_trait::async_trait]
    impl LLMProvider for ScriptedProvider {
        fn name(&self) -> &str {
            "mock"
        }

        async fn complete(&self, _params: &CompletionParams) -> Result<CompletionResult, anyhow::Error> {
            self.temperatures.lock().unwrap().push(_params.temperature);
            // Assign the body at first poll (join_all polls in input order,
            // so this is deterministic) BEFORE parking on the timer.
            let i = self.calls.fetch_add(1, Ordering::SeqCst).min(self.bodies.len() - 1);
            let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(now, Ordering::SeqCst);
            // Park so concurrent samples overlap; under `start_paused` the
            // timer auto-advances once every sample is in flight.
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            Ok(CompletionResult {
                content: self.bodies[i].clone(),
                total_tokens: 1,
                model: "mock".to_string(),
            })
        }
    }

    fn mock_client(bodies: Vec<String>) -> (LLMClient, Arc<AtomicUsize>, Arc<std::sync::Mutex<Vec<f32>>>) {
        let in_flight = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let temperatures = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut registry = ProviderRegistry::new();
        registry.register(Box::new(ScriptedProvider {
            bodies,
            calls: AtomicUsize::new(0),
            in_flight: in_flight.clone(),
            peak: peak.clone(),
            temperatures: temperatures.clone(),
        }));
        (LLMClient::new().with_registry(Arc::new(registry)), peak, temperatures)
    }

    fn mock_configs() -> Vec<crate::models::LLMConfig> {
        vec![crate::models::LLMConfig {
            provider: "mock".to_string(),
            model: "mock-model".to_string(),
            api_key: "k".to_string(),
            api_base: String::new(),
            max_tokens: 4096,
            temperature: 0.3,
            disable_thinking: None,
        }]
    }

    #[tokio::test(start_paused = true)]
    async fn test_sampling_runs_concurrently_and_reports_median() {
        let bodies = vec![
            "score: 70\nsummary: \"a\"\nfindings: []".to_string(),
            "score: 90\nsummary: \"b\"\nfindings: []".to_string(),
            "score: 80\nsummary: \"c\"\nfindings: []".to_string(),
        ];
        let (client, peak, _temps) = mock_client(bodies);
        let call = call_scoring(
            &client,
            &mock_configs(),
            "sys",
            "user",
            3,
            "code_quality",
            CODE_QUALITY_KEYS,
        )
        .await
        .unwrap();
        // join_all preserves input order; scores land in poll order.
        assert_eq!(call.samples, Some(vec![70, 90, 80]));
        assert_eq!(call.median, Some(80));
        assert_eq!(peak.load(Ordering::SeqCst), 3, "samples must overlap, not run serially");
        // Representative content is the lower-middle sample's real response.
        assert!(call.content.contains("summary: \"c\""));
    }

    #[tokio::test(start_paused = true)]
    async fn test_sampling_drops_unparseable_and_scoreless_samples() {
        let bodies = vec![
            "score: 90\nsummary: \"good\"\nfindings: []".to_string(),
            "this is not yaml at all".to_string(), // unparseable → dropped
            "summary: \"no score here\"\nfindings: []".to_string(), // schema-conforming but no score → dropped
            "score: 70\nsummary: \"also good\"\nfindings: []".to_string(),
        ];
        let (client, _peak, _temps) = mock_client(bodies);
        let call = call_scoring(
            &client,
            &mock_configs(),
            "sys",
            "user",
            4,
            "code_quality",
            CODE_QUALITY_KEYS,
        )
        .await
        .unwrap();
        assert_eq!(call.samples, Some(vec![90, 70]));
        assert_eq!(call.median, Some(80));
    }

    #[tokio::test(start_paused = true)]
    async fn test_sampling_all_failed_is_error_for_fallback_path() {
        // No registry → direct HTTP to an unreachable endpoint (connection
        // refused, offline, fail-fast). All samples fail → Err, which the
        // orchestration layer turns into the explicit flagged fallback.
        let client = LLMClient::new();
        let configs = vec![crate::models::LLMConfig {
            provider: "openai".to_string(),
            model: "unreachable".to_string(),
            api_key: "sk-test".to_string(),
            api_base: "http://127.0.0.1:1".to_string(),
            max_tokens: 4096,
            temperature: 0.3,
            disable_thinking: None,
        }];
        let err = call_scoring(
            &client,
            &configs,
            "sys",
            "user",
            3,
            "architecture",
            ARCHITECTURE_LEAD_KEYS,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("all 3 scoring samples failed"));
    }

    // ─── scoring temperature override ─────

    #[test]
    fn test_scoring_configs_runs_cold_and_preserves_input() {
        let mk = |temperature: f32| crate::models::LLMConfig {
            provider: "p".to_string(),
            model: "m".to_string(),
            api_key: "k".to_string(),
            api_base: "https://example.com".to_string(),
            max_tokens: 4096,
            temperature,
            disable_thinking: None,
        };
        let configs = vec![mk(0.3), mk(0.9)];
        let overridden = scoring_configs(&configs);
        assert!(overridden.iter().all(|c| c.temperature == 0.0));
        // The caller's chain keeps its temperatures — the override is
        // per-call, not a mutation of shared state.
        assert_eq!(configs[0].temperature, 0.3);
        assert_eq!(configs[1].temperature, 0.9);
        // The cap invariant: scoring temperature never exceeds 0.2.
        assert!(SCORING_TEMPERATURE <= SCORING_TEMPERATURE_MAX);
    }

    #[tokio::test(start_paused = true)]
    async fn test_scoring_calls_reach_provider_at_zero_temperature() {
        let bodies = vec!["score: 80\nsummary: \"a\"\nfindings: []".to_string()];
        let (client, _peak, temps) = mock_client(bodies);
        let mut configs = mock_configs();
        configs[0].temperature = 0.9; // loud non-zero input to prove the override
        let call = call_scoring(&client, &configs, "sys", "user", 3, "code_quality", CODE_QUALITY_KEYS)
            .await
            .unwrap();
        assert_eq!(call.median, Some(80));
        let temps = temps.lock().unwrap();
        assert_eq!(temps.len(), 3, "every sample is its own provider call");
        assert!(temps.iter().all(|&t| t == 0.0), "scoring must run cold: {temps:?}");
        // Global default untouched: the original config keeps 0.9.
        assert_eq!(configs[0].temperature, 0.9);
    }
