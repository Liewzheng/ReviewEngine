//! Registry of enabled commands and expert selection.
//!
//! The [`CommandRegistry`] maps command names (e.g. "review", "describe")
//! to enabled/disabled status, and provides filtering logic to select
//! which experts should run for a given command based on their trigger
//! type and configured command list. This is Phase 2 scaffolding that
//! will eventually drive the full routing layer. The module also
//! contains test helpers for building sample registries and experts.

use crate::models::*;
use std::collections::HashMap;

/// Registry that maps command names to enabled/disabled status and
/// filters eligible experts for a given command.
pub struct CommandRegistry {
    commands: HashMap<String, bool>,
}

impl CommandRegistry {
    /// Create a new `CommandRegistry` from a command-name → enabled map.
    pub fn new(commands: HashMap<String, bool>) -> Self {
        Self { commands }
    }

    /// Check whether the named command is currently enabled.
    pub fn is_enabled(&self, name: &str) -> bool {
        self.commands.get(name).copied().unwrap_or(false)
    }

    /// Select experts that are eligible for the given command.
    ///
    /// Filters by command enablement and by each expert's `commands` list.
    /// Only enabled experts whose command list includes the given command
    /// (or has an empty command list, implying all commands) are returned.
    pub fn select_experts_for_command<'a>(&self, command: &str, experts: &'a [ExpertDef]) -> Vec<&'a ExpertDef> {
        if !self.is_enabled(command) {
            return vec![];
        }
        experts
            .iter()
            .filter(|e| {
                e.config.enabled
                    && !matches!(e.trigger, ExpertTrigger::OnDemand)
                    && (e.config.commands.is_empty() || e.config.commands.iter().any(|c| c == command))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_registry() -> CommandRegistry {
        let mut cmds = HashMap::new();
        cmds.insert("review".to_string(), true);
        cmds.insert("describe".to_string(), false);
        CommandRegistry::new(cmds)
    }

    fn make_expert(name: &str, commands: Vec<String>, enabled: bool) -> ExpertDef {
        ExpertDef {
            name: name.to_string(),
            trigger: ExpertTrigger::Always,
            prompt: String::new(),
            config: ExpertTomlDef {
                enabled,
                commands,
                ..Default::default()
            },
        }
    }

    #[test]
    fn test_is_enabled_true() {
        let reg = make_registry();
        assert!(reg.is_enabled("review"));
    }

    #[test]
    fn test_is_enabled_false() {
        let reg = make_registry();
        assert!(!reg.is_enabled("describe"));
    }

    #[test]
    fn test_is_enabled_unknown() {
        let reg = make_registry();
        assert!(!reg.is_enabled("nonexistent"));
    }

    #[test]
    fn test_select_experts_returns_matching() {
        let reg = make_registry();
        let experts = vec![
            make_expert("sam", vec!["review".to_string()], true),
            make_expert("jordan", vec!["improve".to_string()], true),
        ];
        let selected = reg.select_experts_for_command("review", &experts);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].name, "sam");
    }

    #[test]
    fn test_select_experts_command_disabled() {
        let reg = make_registry();
        let experts = vec![make_expert("sam", vec!["describe".to_string()], true)];
        let selected = reg.select_experts_for_command("describe", &experts);
        assert!(selected.is_empty());
    }

    #[test]
    fn test_select_experts_no_matching_commands() {
        let reg = make_registry();
        let experts = vec![make_expert("sam", vec!["improve".to_string()], true)];
        let selected = reg.select_experts_for_command("review", &experts);
        assert!(selected.is_empty());
    }

    #[test]
    fn test_select_experts_disabled_expert_with_enabled_command() {
        let reg = make_registry();
        let experts = vec![make_expert("sam", vec!["review".to_string()], false)];
        let selected = reg.select_experts_for_command("review", &experts);
        assert!(selected.is_empty());
    }

    #[test]
    fn test_select_experts_excludes_on_demand() {
        let reg = make_registry();
        let on_demand = ExpertDef {
            name: "sam".to_string(),
            trigger: ExpertTrigger::OnDemand,
            prompt: String::new(),
            config: ExpertTomlDef {
                enabled: true,
                commands: vec!["review".to_string()],
                ..Default::default()
            },
        };
        let experts = [on_demand];
        let selected = reg.select_experts_for_command("review", &experts);
        assert!(selected.is_empty());
    }

    #[test]
    fn test_select_experts_empty_command_list_implies_all_commands() {
        let reg = make_registry();
        let experts = vec![make_expert("sam", vec![], true)];
        let selected = reg.select_experts_for_command("review", &experts);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].name, "sam");
    }
}
