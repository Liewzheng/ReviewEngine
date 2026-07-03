//! Concrete command/action implementations for the review pipeline.
//!
//! Each submodule defines a standalone action (`ask`, `describe`, `improve`,
//! `repo_review`, `update_changelog`, `init`) that encapsulates a specific review
//! operation. These actions are typically driven from the CLI or the server
//! dispatcher and produce structured output such as findings, descriptions,
//! improvement suggestions, or changelog entries. The module serves as a
//! registry of all available action implementations.

pub mod ask;
pub mod describe;
pub mod improve;
pub mod init;
pub mod registry;
pub mod repo_review;
pub mod update_changelog;
