//! CLI command handlers, split by domain into submodules.
//!
//! This facade only declares the submodules and re-exports the `pub` entry
//! points, so `cli::mod` keeps calling `handlers::run_*` unchanged. Shared
//! output/LLM helpers live in `review`/`output` (the `pub(super)` items there
//! are used by the sibling command modules).

pub mod ask;
pub mod changelog;
pub mod config;
pub mod describe;
pub mod improve;
pub mod output;
pub mod review;
pub mod upgrade;
pub mod upgrade_install;

pub use ask::{run_ask, run_ask_local_diff, run_ask_local_repo, run_ask_stdin};
pub use changelog::run_update_changelog;
pub use config::watch_config_file;
pub use describe::{run_describe, run_describe_local_diff, run_describe_local_repo};
pub use improve::{run_improve, run_improve_local_diff, run_improve_local_repo};
pub use review::{
    resolve_llm_configs, run_local, run_local_path, run_local_repo, run_mr, run_repo_review_local_or_enhanced,
    run_stdin,
};
pub use upgrade::run_upgrade;
