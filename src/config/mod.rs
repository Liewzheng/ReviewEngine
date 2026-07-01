//! Configuration loading, parsing, and merging for the review engine.
//!
//! This module is part of the review-engine CodeReview Board platform.
//!
//! Provides [`default_config`] (built from an embedded default TOML),
//! [`parse_toml`] for deserialising user-provided TOML strings into
//! [`AppConfig`], and [`merge_default`] which combines a user config
//! with the defaults (missing fields fall back to defaults, lists are
//! extended, and expert/command maps are also merged). Also supports
//! environment-variable overrides and precedence rules for CLI flags
//! vs. config-file values.

pub mod defaults;
pub mod resolver;

pub use defaults::*;
pub use resolver::*;
