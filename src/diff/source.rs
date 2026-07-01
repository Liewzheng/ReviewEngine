use serde::{Deserialize, Serialize};

/// Describes the source of a diff when reviewing a local git repository.
///
/// Each variant corresponds to a different `git diff` invocation pattern.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum LocalDiffSource {
    /// Diff between the working tree and a base reference (e.g. `git diff main`).
    WorkingTreeVsRef {
        /// The base branch or commit to compare against.
        base_ref: String,
    },
    /// Diff of staged changes only (`git diff --cached`).
    Staged,
    /// Diff between two arbitrary commits (`git diff <since>..<until>`).
    Commits {
        /// The start of the commit range (older).
        since: String,
        /// The end of the commit range (newer).
        until: String,
    },
}
