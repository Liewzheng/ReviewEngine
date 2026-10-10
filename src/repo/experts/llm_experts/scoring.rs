//! Shared scoring helpers: YAML parsing, temperature override, sample counting.

/// Maximum bytes of a raw LLM response kept in a parse-failure warning.
pub(crate) const EXCERPT_MAX_BYTES: usize = 300;

/// Keys that mark a CodeQuality response as schema-conforming.
pub(crate) const CODE_QUALITY_KEYS: &[&str] = &["score", "summary", "findings"];

/// Keys that mark an ArchitectureLead response as schema-conforming.
pub(crate) const ARCHITECTURE_LEAD_KEYS: &[&str] = &["score", "summary", "risk_areas", "guidance", "focus_modules"];

/// Legacy schema: an older prompt template wrapped the LLM's payload in a
/// top-level `review:` mapping (e.g. `review: { score: ..., findings: [...] }`)
/// instead of putting `score` / `summary` / `findings` at the root. M18-W2
/// accepts this as a fallback so a config that's still pinning the old
/// prompt template does not silently degrade to a 70/0 placeholder; the
/// recovered payload is logged at INFO with `reason_code =
/// "legacy_review_fallback"` so the user can see the migration surface.
pub(crate) const LEGACY_REVIEW_FINDINGS_KEYS: &[&str] = &["findings"];

pub(crate) const SCORING_TEMPERATURE: f32 = 0.0;
pub(crate) const SCORING_TEMPERATURE_MAX: f32 = 0.2;

pub(crate) fn scoring_configs(configs: &[crate::models::LLMConfig]) -> Vec<crate::models::LLMConfig> {
    configs
        .iter()
        .cloned()
        .map(|mut c| {
            c.temperature = SCORING_TEMPERATURE.clamp(0.0, SCORING_TEMPERATURE_MAX);
            c
        })
        .collect()
}

/// Stable reason codes paired with `parse_expert_yaml`'s fallback paths
/// (M18-W2). Logged at INFO alongside the WARN-level excerpt so dashboards
/// can filter on `reason_code` without having to substring-match on the
/// human-readable message — a sub-string `connection` in a 401 body
/// (RENG-35) is the same class of fragility these codes exist to avoid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ParseReasonCode {
    /// The LLM returned an empty (or whitespace-only) response.
    EmptyResponse,
    /// Parsed YAML had the right top-level mapping but none of the
    /// schema-conforming `expected_keys` were present.
    MissingExpectedKeys,
    /// `serde_yaml_ng` rejected the cleaned response (malformed YAML).
    UnparseableYaml,
    /// M18-W2: the response only carried findings inside a nested
    /// `review:` mapping — a legacy prompt template. Recovered by
    /// promoting the inner mapping to the top level.
    LegacyReviewFallback,
}

impl ParseReasonCode {
    /// Stable lowercase string for log filters.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ParseReasonCode::EmptyResponse => "empty_response",
            ParseReasonCode::MissingExpectedKeys => "missing_expected_keys",
            ParseReasonCode::UnparseableYaml => "unparseable_yaml",
            ParseReasonCode::LegacyReviewFallback => "legacy_review_fallback",
        }
    }
}

pub(crate) fn parse_expert_yaml(expert: &str, raw: &str, expected_keys: &[&str]) -> serde_yaml_ng::Value {
    if raw.trim().is_empty() {
        tracing::warn!(
            expert_name = expert,
            reason_code = ParseReasonCode::EmptyResponse.as_str(),
            "LLM returned empty response; using fallback score and empty findings"
        );
        return serde_yaml_ng::Value::Null;
    }

    let cleaned = crate::output::parser::clean_yaml(raw);
    let parsed = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(&cleaned).ok();
    if let Some(v) = &parsed {
        if v.is_mapping() && expected_keys.iter().any(|k| !v[*k].is_null()) {
            return v.clone();
        }
    }
    if let Some(fenced) = crate::output::parser::extract_first_fenced_yaml(raw) {
        if let Ok(v) = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(&fenced) {
            if v.is_mapping() && expected_keys.iter().any(|k| !v[*k].is_null()) {
                return v;
            }
        }
    }

    // M18-W2: accept the legacy `review: { ... }` wrapper as a fallback
    // when the modern top-level keys are absent. Promotes the inner
    // mapping so `value["findings"]` / `value["score"]` work the same way
    // for downstream consumers. Logged at INFO with `reason_code =
    // "legacy_review_fallback"` so config migrations are observable.
    if let Some(legacy) = legacy_review_payload(parsed.as_ref()) {
        tracing::info!(
            expert_name = expert,
            reason_code = ParseReasonCode::LegacyReviewFallback.as_str(),
            "LLM response uses the legacy 'review:' wrapper; promoting to top-level keys"
        );
        return legacy;
    }

    if parsed.as_ref().is_some_and(|v| v.is_mapping()) {
        tracing::warn!(
            expert_name = expert,
            reason_code = ParseReasonCode::MissingExpectedKeys.as_str(),
            raw_len = raw.len(),
            excerpt = %truncate_excerpt(raw),
            "LLM response parsed as YAML but missing expected keys {expected_keys:?}; using fallback score and empty findings"
        );
    } else {
        tracing::warn!(
            expert_name = expert,
            reason_code = ParseReasonCode::UnparseableYaml.as_str(),
            raw_len = raw.len(),
            excerpt = %truncate_excerpt(raw),
            "LLM response failed YAML parse; using fallback score and empty findings"
        );
    }
    serde_yaml_ng::Value::Null
}

/// M18-W2 legacy schema recovery: when the cleaned YAML parses into a
/// mapping whose only meaningful content is under a top-level `review:`
/// key (the old prompt-template wrapper), promote that inner mapping so
/// `value["findings"]` / `value["score"]` work the same way the rest of
/// the parsing code already does. Only fires when the modern top-level
/// keys (`expected_keys`) are absent — a payload that has both surfaces
/// keeps the modern form, which is what the rest of the pipeline
/// already validated against.
///
/// `None` when the modern top-level keys ARE present (already handled
/// upstream), when there's no `review:` wrapper at all, or when the
/// wrapper exists but its inner mapping carries no `findings` /
/// `score` (an empty shell — same effect as no legacy payload).
fn legacy_review_payload(parsed: Option<&serde_yaml_ng::Value>) -> Option<serde_yaml_ng::Value> {
    let mapping = parsed?.as_mapping()?;
    let review = mapping
        .get(serde_yaml_ng::Value::String("review".into()))?
        .as_mapping()?;
    let promoted = serde_yaml_ng::Value::Mapping(review.clone());
    if LEGACY_REVIEW_FINDINGS_KEYS.iter().any(|k| !promoted[*k].is_null()) {
        Some(promoted)
    } else {
        None
    }
}

pub(crate) fn truncate_excerpt(raw: &str) -> String {
    let mut excerpt = String::new();
    for ch in raw.chars() {
        if excerpt.len() + ch.len_utf8() > EXCERPT_MAX_BYTES {
            break;
        }
        excerpt.push(ch);
    }
    excerpt.replace('\n', "\\n")
}

pub(crate) fn median_score(samples: &[u8]) -> Option<u8> {
    if samples.is_empty() {
        return None;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let mid = sorted.len() / 2;
    if sorted.len() % 2 == 1 {
        Some(sorted[mid])
    } else {
        Some(((u16::from(sorted[mid - 1]) + u16::from(sorted[mid])).div_ceil(2)) as u8)
    }
}

pub(crate) fn scoring_sample_count(config: Option<&crate::models::AppConfig>) -> usize {
    config.map(|c| c.scoring.score_samples).unwrap_or(1).max(1)
}

#[derive(Debug)]
pub(crate) struct ScoringCall {
    pub content: String,
    pub median: Option<u8>,
    pub samples: Option<Vec<u8>>,
    /// M18-W2: total number of scoring samples that landed (whether they
    /// produced a score or not). When `samples.is_some()`, this is the
    /// `n` requested; when `samples` is `None` (the single-shot `n = 1`
    /// path) the value is kept as a uniform `1` baseline so the
    /// multi/single-shot shapes line up internally, but the public
    /// ExpertScore field is gated on `samples.is_some()` (see
    /// `code_quality.rs` / `architecture.rs`). Used to compute the
    /// by-expert fallback rate as `samples_failed / samples_attempted`.
    pub samples_attempted: usize,
    /// M18-W2: how many of the attempted samples failed (call error,
    /// unparseable YAML, or schema-conforming YAML with no `score`).
    /// When `samples_attempted > 0`, the fallback rate is
    /// `samples_failed / samples_attempted`. `0` on the single-shot path
    /// (`n = 1`) because a call error short-circuits through `?` and
    /// never reaches this struct (the unreachable "1 when errored"
    /// wording from r1 is gone — see `call_scoring`).
    pub samples_failed: usize,
}

pub(crate) async fn call_scoring(
    llm: &crate::llm::client::LLMClient,
    configs: &[crate::models::LLMConfig],
    system: &str,
    user: &str,
    n: usize,
    expert: &str,
    expected_keys: &[&str],
) -> anyhow::Result<ScoringCall> {
    let overridden = scoring_configs(configs);
    let configs = overridden.as_slice();

    if n <= 1 {
        let response = llm.complete_with_fallback(configs, system, user).await?;
        // M18-W2: the single-shot path always returns a 1/0 baseline.
        // Call errors short-circuit through the `?` above and never reach
        // this branch, so the unreachable "samples_failed = 1 when the
        // call errored" wording from the r1 draft is gone — there is no
        // errored-but-`Ok` path here. The two ExpertScore construction
        // sites (code_quality.rs / architecture.rs) gate the public
        // `samples_attempted` / `samples_failed` fields on
        // `call.samples.is_some()`, so the JSON contract holds:
        // single-shot runs report no rate; multi-sample runs do.
        return Ok(ScoringCall {
            content: response.content,
            median: None,
            samples: None,
            samples_attempted: 1,
            samples_failed: 0,
        });
    }

    let calls: Vec<_> = (0..n)
        .map(|_| llm.complete_with_fallback(configs, system, user))
        .collect();
    let results = futures::future::join_all(calls).await;

    let mut scored: Vec<(u8, String)> = Vec::new();
    let mut failed: usize = 0;
    for result in results {
        match result {
            Ok(response) => {
                let value = parse_expert_yaml(expert, &response.content, expected_keys);
                match value["score"].as_u64() {
                    Some(raw) => scored.push((raw.min(100) as u8, response.content)),
                    None => {
                        failed += 1;
                        tracing::warn!(
                            expert_name = expert,
                            reason_code = ParseReasonCode::MissingExpectedKeys.as_str(),
                            "scoring sample dropped: no genuine score in response"
                        );
                    }
                }
            }
            Err(e) => {
                failed += 1;
                tracing::warn!(
                    expert_name = expert,
                    error = %e,
                    "scoring sample dropped: call failed"
                );
            }
        }
    }
    // M18-W2: publish the fallback rate at INFO when at least one sample
    // failed. The `samples` field below carries the surviving scores; the
    // pair `samples_failed` / `samples_attempted` is the by-expert rate
    // dashboards can aggregate (e.g. over a week of reviews).
    if failed > 0 {
        tracing::info!(
            expert_name = expert,
            reason_code = "scoring_sample_fallback",
            samples_attempted = n,
            samples_failed = failed,
            samples_succeeded = scored.len(),
            fallback_rate = format!("{:.2}", failed as f64 / n as f64),
            "scoring sample fallback rate (M18-W2)"
        );
    }
    if scored.is_empty() {
        anyhow::bail!("all {n} scoring samples failed for expert '{expert}'");
    }

    let samples: Vec<u8> = scored.iter().map(|(s, _)| *s).collect();
    let median = match median_score(&samples) {
        Some(median) => median,
        None => anyhow::bail!("no usable scoring samples for expert '{expert}'"),
    };
    scored.sort_by_key(|(s, _)| *s);
    let representative = scored[(scored.len() - 1) / 2].1.clone();
    Ok(ScoringCall {
        content: representative,
        median: Some(median),
        samples: Some(samples),
        samples_attempted: n,
        samples_failed: failed,
    })
}

pub(crate) fn append_facts_block(user: &mut String, facts_block: Option<&str>) {
    if let Some(facts) = facts_block {
        user.push_str("\n\n## Repository Facts (deterministic static analysis)\n");
        user.push_str(facts);
    }
}
