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

use serde::Serialize;

use crate::models::AppConfig;

/// Highest expert weight the schema defines: `ExpertTomlDef::weight` is
/// documented as 0–100 and the config-file validator enforces the enabled
/// weights summing to exactly 100. The WebUI slider is bounded by it too, so
/// anything above it can only come from a hand-written client or a hand-edited
/// database row — both handled below (rejected at the API boundary, dropped
/// when read back).
pub const MAX_EXPERT_WEIGHT: u8 = 100;

/// The fields `PUT /api/v1/system/experts/{id}` lets the UI change. Every
/// field is optional: a `None` means "this request did not touch it", so a
/// partially-specified override never clobbers the other field.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ExpertOverride {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
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
/// is the value of the `app_settings` row. Only [`Serialize`] is derived:
/// deserialization goes through [`ExpertOverrides::from_setting`] alone, which
/// is the one place that can drop an out-of-range weight instead of storing it
/// (a derived `Deserialize` would happily accept `{"weight": 200}`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
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
    /// override that changes nothing" are the same state, so an entry is never
    /// stored empty (see [`Self::to_setting`]).
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
    ///
    /// Empty entries are pruned defensively: `record` and [`Self::from_setting`]
    /// both avoid creating one, and a stored `{}` would be indistinguishable
    /// from noise on a later read.
    pub fn to_setting(&self) -> serde_json::Value {
        let mut pruned = self.clone();
        pruned.entries.retain(|_, over| !over.is_empty());
        serde_json::to_value(&pruned).unwrap_or_else(|_| serde_json::json!({}))
    }

    /// Parse a stored `app_settings` value.
    ///
    /// This is the ONLY way into the type, and it is deliberately lenient and
    /// infallible: the row may have been hand-edited (or written by an older
    /// build), and a bad row must degrade to "no overrides" with a WARN rather
    /// than take the server down at startup — a `serve` that cannot boot even
    /// has no Web UI left to fix the row from.
    ///
    /// Dropped, each with a WARN: a value that is not a JSON object at all, an
    /// entry that is not an object, an `enabled` that is not a bool, and a
    /// `weight` that is not an integer in `0..=MAX_EXPERT_WEIGHT` (so a
    /// hand-edited row cannot inject a weight the schema forbids). An entry
    /// left with no valid field is dropped entirely — "no override" must not be
    /// stored as `{}`.
    ///
    /// A dropped field is NOT a silent loss of the effective value: the
    /// override simply stops covering it, so the config file's value stands —
    /// the same "unset is unset" rule the other surfaces follow.
    pub fn from_setting(value: &serde_json::Value) -> Self {
        if value.is_null() {
            return Self::default();
        }
        let Some(entries) = value.as_object() else {
            tracing::warn!(
                found = json_kind(value),
                "ignoring the persisted expert overrides: the app_settings row 'experts' must be \
                 a JSON object of expert name → override; the config file's [review_experts] \
                 values stand"
            );
            return Self::default();
        };

        let mut overrides = Self::default();
        for (name, raw) in entries {
            let Some(fields) = raw.as_object() else {
                tracing::warn!(
                    expert = %name,
                    "ignoring malformed persisted expert override: not a JSON object"
                );
                continue;
            };

            let mut patch = ExpertOverride::default();
            if let Some(enabled) = fields.get("enabled") {
                match enabled.as_bool() {
                    Some(b) => patch.enabled = Some(b),
                    None => tracing::warn!(
                        expert = %name,
                        "ignoring persisted expert override field 'enabled': expected a boolean"
                    ),
                }
            }
            if let Some(weight) = fields.get("weight") {
                match weight
                    .as_u64()
                    .and_then(|w| u8::try_from(w).ok())
                    .filter(|w| *w <= MAX_EXPERT_WEIGHT)
                {
                    Some(w) => patch.weight = Some(w),
                    None => tracing::warn!(
                        expert = %name,
                        value = %weight,
                        max = MAX_EXPERT_WEIGHT,
                        "ignoring persisted expert override 'weight': expected an integer 0–{}; \
                         the config file's value stands",
                        MAX_EXPERT_WEIGHT
                    ),
                }
            }
            // A patch with no valid field leaves no entry behind: "no override"
            // must not be stored as an empty object.
            overrides.record(name, patch);
        }
        overrides
    }
}

/// The JSON kind of a value, for error messages that say what arrived instead
/// of only what was expected.
fn json_kind(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "a boolean",
        serde_json::Value::Number(_) => "a number",
        serde_json::Value::String(_) => "a string",
        serde_json::Value::Array(_) => "an array",
        serde_json::Value::Object(_) => "an object",
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

        // An all-`None` patch changes nothing (the earlier fields survive)…
        overrides.record("security", ExpertOverride::default());
        let entry = overrides.get("security").expect("entry keeps its fields");
        assert_eq!(entry.enabled, Some(false));
        assert_eq!(entry.weight, Some(40));

        // …and on a name with no entry it must not create an empty one.
        overrides.record("ghost", ExpertOverride::default());
        assert_eq!(overrides.get("ghost"), None, "an empty patch stores nothing");
        assert_eq!(overrides.len(), 1, "no empty entry was created");
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
        assert_eq!(ExpertOverrides::from_setting(&value), overrides);
        assert_eq!(
            ExpertOverrides::from_setting(&serde_json::json!(null)),
            ExpertOverrides::default()
        );
    }

    /// A hand-edited row cannot inject a weight the schema does not allow: the
    /// field is dropped (the config file's value stands) instead of being
    /// applied, and the rest of the entry survives.
    #[test]
    fn from_setting_drops_an_out_of_range_weight() {
        let value = serde_json::json!({
            "security": { "enabled": false, "weight": 200 },
            "docs": { "weight": 101 },
            "quality": { "weight": 100 }
        });
        let overrides = ExpertOverrides::from_setting(&value);

        let security = overrides.get("security").expect("enabled survives");
        assert_eq!(security.enabled, Some(false));
        assert_eq!(security.weight, None, "the out-of-range weight is dropped");
        assert_eq!(
            overrides.get("docs"),
            None,
            "an entry whose only field was invalid leaves no entry behind"
        );
        assert_eq!(
            overrides.get("quality").and_then(|o| o.weight),
            Some(MAX_EXPERT_WEIGHT),
            "the last valid weight is kept"
        );

        // The dropped weight leaves the file's value in force.
        let mut cfg = config_with(&[("security", true, 50), ("docs", true, 50)]);
        assert_eq!(overrides.apply_to(&mut cfg), 1);
        assert_eq!(cfg.review_experts["security"].weight, 50);
        assert!(!cfg.review_experts["security"].enabled);
        assert_eq!(cfg.review_experts["docs"].weight, 50);
    }

    /// Wrong types (and entries that are not objects) are dropped field by
    /// field: one bad field must not discard the whole row, and one bad entry
    /// must not discard the others.
    #[test]
    fn from_setting_ignores_malformed_entries() {
        let value = serde_json::json!({
            "scalar-entry": 5,
            "string-entry": "disabled",
            "stringly-typed": { "enabled": "yes", "weight": "30" },
            "mixed": { "enabled": true, "weight": "30" },
            "negative": { "weight": -5 },
            "fractional": { "weight": 12.5 }
        });
        let overrides = ExpertOverrides::from_setting(&value);

        for name in [
            "scalar-entry",
            "string-entry",
            "stringly-typed",
            "negative",
            "fractional",
        ] {
            assert_eq!(overrides.get(name), None, "'{name}' must be dropped, not applied");
        }
        assert_eq!(
            overrides.get("mixed"),
            Some(&ExpertOverride {
                enabled: Some(true),
                weight: None,
            }),
            "a valid field survives its invalid sibling"
        );
        assert_eq!(overrides.len(), 1);
    }

    /// A row that is not an override map at all degrades to "no overrides"
    /// with a WARN — never a panic, never a half-applied map, and never a
    /// startup failure (the WARN is asserted in the persist-level test, which
    /// is where the startup path is exercised).
    #[test]
    fn from_setting_ignores_a_non_object_row() {
        for value in [
            serde_json::json!([1, 2]),
            serde_json::json!("nope"),
            serde_json::json!(7),
        ] {
            assert_eq!(
                ExpertOverrides::from_setting(&value),
                ExpertOverrides::default(),
                "{value} must degrade to no overrides"
            );
        }
    }

    /// "No override" is never stored as an empty object — neither for a row
    /// the UI emptied nor for a hand-written `{}` entry.
    #[test]
    fn to_setting_never_carries_an_empty_entry() {
        let value = serde_json::json!({ "emptied": {}, "invalid": { "weight": 200 } });
        let overrides = ExpertOverrides::from_setting(&value);

        assert_eq!(overrides.to_setting(), serde_json::json!({}));
        assert!(overrides.is_empty());
    }
}
