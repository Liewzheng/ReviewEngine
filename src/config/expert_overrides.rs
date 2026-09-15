//! WebUI-managed expert overrides (RENG-69).
//!
//! `PUT /api/v1/system/experts/{id}` edits an expert's `enabled` / `weight`
//! at runtime. Before 0.10.24 that mutation lived in `AppState::app_config`
//! only, so a container recreate restored the config file's `[review_experts]`
//! values and the WebUI edit was silently lost.
//!
//! This type is the persisted form of those edits: a map of
//! `expert name → patched fields`, so it is self-describing (it holds ONLY
//! what the UI changed, never a snapshot of the resolved expert) and can be
//! replayed over any base config.
//!
//! Precedence on startup: `config file [review_experts] < DB overrides` — the
//! same DB-over-file rule the other config surfaces follow (LLM providers,
//! git platforms). The config file stays the base/default: an override patches
//! an entry that EXISTS there, it never adds a new expert, so removing an
//! expert from the file also removes its override (the override is skipped
//! with a debug log).
//!
//! Storage: the `app_settings` row key `experts` (see
//! [`crate::server::api::config::persist::save_expert_overrides`]). A separate
//! key rather than an extra section of the `ui` projection: the `ui` row is a
//! `UiConfig` replayed through the `PUT /config` pipeline, and experts are not
//! reachable from that endpoint at all — folding them in would mean
//! fabricating a config-file patch shape for them.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::models::AppConfig;

/// The fields `PUT /api/v1/system/experts/{id}` lets the UI change. Every
/// field is optional: a `None` means "this request did not touch it", so a
/// partially-specified override never clobbers the other field.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpertOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<u8>,
}

impl ExpertOverride {
    /// True when the patch carries nothing (its stored entry can be dropped).
    pub fn is_empty(&self) -> bool {
        self.enabled.is_none() && self.weight.is_none()
    }
}

/// Persisted expert overrides, keyed by the expert's name in
/// `AppConfig::review_experts` (NOT the slugified UI id: the name is the key
/// the config file itself uses, so the two never drift).
///
/// Serializes as a bare JSON object (`{"<name>": {"enabled": false}}`) — this
/// is the value of the `app_settings` row.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ExpertOverrides {
    entries: BTreeMap<String, ExpertOverride>,
}

impl ExpertOverrides {
    /// True when nothing was ever overridden.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Number of overridden experts.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// The override of one expert, `None` when the UI never changed it.
    pub fn get(&self, name: &str) -> Option<&ExpertOverride> {
        self.entries.get(name)
    }

    /// All overrides in deterministic (name-sorted) order.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &ExpertOverride)> {
        self.entries.iter()
    }

    /// Merge one UI edit into the map. Fields the request omitted
    /// (`None`) leave the stored value untouched, and a patch that clears
    /// both fields removes the entry entirely — "no override" and "an
    /// override that changes nothing" are the same state.
    pub fn record(&mut self, name: &str, patch: ExpertOverride) {
        let entry = self.entries.entry(name.to_string()).or_default();
        if patch.enabled.is_some() {
            entry.enabled = patch.enabled;
        }
        if patch.weight.is_some() {
            entry.weight = patch.weight;
        }
        if entry.is_empty() {
            self.entries.remove(name);
        }
    }

    /// Apply every override onto a freshly resolved config, in place.
    ///
    /// Returns the number of experts actually patched. Overrides naming an
    /// expert the config does not define are skipped with a debug log — the
    /// config file stays the base, so an override can patch an entry but
    /// never create one.
    pub fn apply_to(&self, cfg: &mut AppConfig) -> usize {
        let mut applied = 0;
        for (name, over) in &self.entries {
            match cfg.review_experts.get_mut(name) {
                Some(expert) => {
                    if let Some(enabled) = over.enabled {
                        expert.enabled = enabled;
                    }
                    if let Some(weight) = over.weight {
                        expert.weight = weight;
                    }
                    applied += 1;
                }
                None => tracing::debug!(
                    expert = %name,
                    "persisted expert override has no matching [review_experts] entry; skipping"
                ),
            }
        }
        applied
    }

    /// The overrides as a storage value (`app_settings` row payload).
    pub fn to_setting(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_else(|_| serde_json::json!({}))
    }

    /// Parse a stored `app_settings` value. Tolerant of a hand-edited row:
    /// unknown fields are ignored, a missing/`null` value is the empty map.
    pub fn from_setting(value: &serde_json::Value) -> anyhow::Result<Self> {
        if value.is_null() {
            return Ok(Self::default());
        }
        serde_json::from_value(value.clone())
            .map_err(|e| anyhow::anyhow!("app_settings row is not an expert-override map: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ExpertTomlDef;

    fn expert(enabled: bool, weight: u8) -> ExpertTomlDef {
        ExpertTomlDef {
            enabled,
            weight,
            ..Default::default()
        }
    }

    fn config_with(entries: &[(&str, bool, u8)]) -> AppConfig {
        let mut experts = std::collections::HashMap::new();
        for (name, enabled, weight) in entries {
            experts.insert((*name).to_string(), expert(*enabled, *weight));
        }
        let mut cfg: AppConfig =
            serde_json::from_value(serde_json::json!({})).expect("empty AppConfig must deserialize");
        cfg.review_experts = experts;
        cfg
    }

    #[test]
    fn apply_patches_only_named_fields_and_leaves_others_alone() {
        let mut overrides = ExpertOverrides::default();
        overrides.record(
            "security",
            ExpertOverride {
                enabled: Some(false),
                weight: None,
            },
        );
        overrides.record(
            "quality",
            ExpertOverride {
                enabled: None,
                weight: Some(30),
            },
        );

        let mut cfg = config_with(&[("security", true, 50), ("quality", true, 20), ("docs", true, 30)]);
        assert_eq!(overrides.apply_to(&mut cfg), 2);

        assert!(!cfg.review_experts["security"].enabled, "enabled patched");
        assert_eq!(cfg.review_experts["security"].weight, 50, "weight untouched");
        assert!(cfg.review_experts["quality"].enabled, "enabled untouched");
        assert_eq!(cfg.review_experts["quality"].weight, 30, "weight patched");
        assert!(
            cfg.review_experts["docs"].enabled,
            "unedited expert keeps the file value"
        );
        assert_eq!(
            cfg.review_experts["docs"].weight, 30,
            "unedited expert keeps the file value"
        );
    }

    #[test]
    fn apply_skips_overrides_without_a_matching_file_entry() {
        let mut overrides = ExpertOverrides::default();
        overrides.record(
            "ghost",
            ExpertOverride {
                enabled: Some(false),
                weight: None,
            },
        );

        let mut cfg = config_with(&[("security", true, 50)]);
        assert_eq!(overrides.apply_to(&mut cfg), 0);
        assert_eq!(cfg.review_experts.len(), 1, "an override never creates an expert");
        assert!(cfg.review_experts["security"].enabled);
    }

    #[test]
    fn record_merges_and_drops_empty_patches() {
        let mut overrides = ExpertOverrides::default();
        overrides.record(
            "security",
            ExpertOverride {
                enabled: Some(false),
                weight: None,
            },
        );
        overrides.record(
            "security",
            ExpertOverride {
                enabled: None,
                weight: Some(40),
            },
        );

        let entry = overrides.get("security").expect("entry");
        assert_eq!(entry.enabled, Some(false), "the earlier field survives");
        assert_eq!(entry.weight, Some(40));

        overrides.record("security", ExpertOverride::default());
        assert!(!overrides.is_empty(), "the merged entry is not empty yet");
        let mut clear = ExpertOverride {
            enabled: None,
            weight: None,
        };
        clear.enabled = Some(true);
        clear.weight = Some(40);
        overrides.record("security", clear);
        assert_eq!(overrides.get("security"), Some(&clear));
    }

    #[test]
    fn setting_round_trip_is_a_bare_name_keyed_object() {
        let mut overrides = ExpertOverrides::default();
        overrides.record(
            "security",
            ExpertOverride {
                enabled: Some(false),
                weight: Some(15),
            },
        );

        let value = overrides.to_setting();
        assert_eq!(value["security"]["enabled"], false);
        assert_eq!(value["security"]["weight"], 15);
        assert_eq!(ExpertOverrides::from_setting(&value).unwrap(), overrides);
        assert_eq!(
            ExpertOverrides::from_setting(&serde_json::json!(null)).unwrap(),
            ExpertOverrides::default()
        );
    }
}
