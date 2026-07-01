//! Diff processing pipeline: parsing, filtering, chunking, and analysis.
//!
//! The `parser` submodule parses raw unified-diff text into structured
//! hunks. The `filter` submodule excludes irrelevant files (generated,
//! vendored, binary). The `chunker` submodule splits large diffs into
//! manageable pieces for token-limited LLM experts. The `large_pr`
//! submodule detects oversized PRs and triggers compression strategies.
//! The `processor` submodule orchestrates the full pipeline, and
//! `source` manages diff provenance from local Git or remote providers.

pub mod chunker;
pub mod filter;
pub mod large_pr;
pub mod parser;
pub mod processor;
pub mod source;
