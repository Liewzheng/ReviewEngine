//! Prompt construction for LLM-based review experts.
//!
//! The [`engine`] submodule provides [`PromptEngine`], which builds
//! the system and user prompts sent to LLM experts, including review
//! prompts, aggregator prompts, and lead-reviewer prompts. The
//! [`templates`] submodule holds all template string constants.
//! Templates are parameterised with MR context, diff content, and
//! expert instructions. The `schemas` submodule defines the expected
//! output schemas that the LLM is asked to produce (e.g. JSON structures).

pub mod engine;
pub mod schemas;
pub mod templates;

pub use engine::*;
