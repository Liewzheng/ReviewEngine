//! LLM-powered repo-level experts for architecture and code quality analysis.

mod architecture;
mod code_quality;
// M38: expose the `scoring` helpers (`ParseReasonCode`,
// `parse_expert_yaml_diagnosed`) so the upstream `ExpertScore` can carry
// the reason code without leaking the module's internals to the public
// surface. `pub(crate)` keeps it inside the crate — the only consumer
// is `crate::repo::experts::ExpertScore`.
pub(crate) mod scoring;
#[cfg(test)]
mod tests;

pub use architecture::ArchitectureLead;
pub use code_quality::CodeQuality;
