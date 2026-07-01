use serde::{Deserialize, Serialize};

/// Variables for the review prompt template
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewPromptVars {
    pub perspective: String,
    pub language: String,
    pub max_findings: usize,
    pub title: String,
    pub branch: String,
    pub description: String,
    pub diff: String,
}
