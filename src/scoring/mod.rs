//! Unified scoring module.
//!
//! Provides two sub-modules:
//! - [`review`] — MR/PR review scoring from expert findings severity.
//! - [`repo`] — repository health scoring from file metrics.

pub mod repo;
pub mod review;

pub use repo::{score_repository, ActionItem, RepoScore, RiskItem};
pub use review::{
    compute_overall_with_config, expert_score, expert_score_with_config, score_to_risk_level_with_config,
    weighted_overall_score, ExpertScoreRecord, ReviewScoreRecord,
};
