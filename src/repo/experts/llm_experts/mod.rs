//! LLM-powered repo-level experts for architecture and code quality analysis.

mod architecture;
mod code_quality;
mod scoring;
#[cfg(test)]
mod tests;

pub use architecture::ArchitectureLead;
pub use code_quality::CodeQuality;
