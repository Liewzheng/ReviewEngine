pub(crate) mod app;
pub mod commands;
#[cfg(test)]
mod tests;

#[cfg(feature = "cli")]
pub mod handlers;

pub use app::{cli_command, run};
