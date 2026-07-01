//! Output formatting, parsing, and rendering of review results.
//!
//! The `parser` submodule parses raw LLM responses into structured
//! [`ExpertReport`] and [`AggregatedReport`] objects. The `renderer`
//! submodule formats reports into Markdown for human consumption or
//! for posting as MR discussions. The `team_renderer` submodule handles
//! multi-expert, consolidated team reports. The `markdown` submodule
//! provides sanitization helpers for LLM-generated Markdown content.
//! The `path` submodule manages output directory and file path conventions.

pub mod markdown;
pub mod parser;
pub mod path;
pub mod renderer;
pub mod team_renderer;
