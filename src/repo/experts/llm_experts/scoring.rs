//! Shared scoring helpers: YAML parsing, temperature override, sample counting.

/// Maximum bytes of a raw LLM response kept in a parse-failure warning.
pub(crate) const EXCERPT_MAX_BYTES: usize = 300;

/// Keys that mark a CodeQuality response as schema-conforming.
pub(crate) const CODE_QUALITY_KEYS: &[&str] = &["score", "summary", "findings"];

/// Keys that mark an ArchitectureLead response as schema-conforming.
pub(crate) const ARCHITECTURE_LEAD_KEYS: &[&str] = &["score", "summary", "risk_areas", "guidance", "focus_modules"];

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

pub(crate) fn parse_expert_yaml(expert: &str, raw: &str, expected_keys: &[&str]) -> serde_yaml_ng::Value {
    if raw.trim().is_empty() {
        tracing::warn!(
            expert_name = expert,
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

    if parsed.as_ref().is_some_and(|v| v.is_mapping()) {
        tracing::warn!(
            expert_name = expert,
            raw_len = raw.len(),
            excerpt = %truncate_excerpt(raw),
            "LLM response parsed as YAML but missing expected keys {expected_keys:?}; using fallback score and empty findings"
        );
    } else {
        tracing::warn!(
            expert_name = expert,
            raw_len = raw.len(),
            excerpt = %truncate_excerpt(raw),
            "LLM response failed YAML parse; using fallback score and empty findings"
        );
    }
    serde_yaml_ng::Value::Null
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
        return Ok(ScoringCall {
            content: response.content,
            median: None,
            samples: None,
        });
    }

    let calls: Vec<_> = (0..n)
        .map(|_| llm.complete_with_fallback(configs, system, user))
        .collect();
    let results = futures::future::join_all(calls).await;

    let mut scored: Vec<(u8, String)> = Vec::new();
    for result in results {
        match result {
            Ok(response) => {
                let value = parse_expert_yaml(expert, &response.content, expected_keys);
                match value["score"].as_u64() {
                    Some(raw) => scored.push((raw.min(100) as u8, response.content)),
                    None => tracing::warn!(
                        expert_name = expert,
                        "scoring sample dropped: no genuine score in response"
                    ),
                }
            }
            Err(e) => tracing::warn!(
                expert_name = expert,
                error = %e,
                "scoring sample dropped: call failed"
            ),
        }
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
    })
}

pub(crate) fn append_facts_block(user: &mut String, facts_block: Option<&str>) {
    if let Some(facts) = facts_block {
        user.push_str("\n\n## Repository Facts (deterministic static analysis)\n");
        user.push_str(facts);
    }
}
