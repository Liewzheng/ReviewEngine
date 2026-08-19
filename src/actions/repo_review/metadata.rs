use super::types::*;
use crate::models::*;
use crate::repo::FileEntry;

// ── Provenance helpers ──
// Every helper here is fail-open: provenance annotates a report, it must
// never abort one.

/// Return the HEAD commit SHA of the Git repository at `root`, or `None`
/// when `root` is not a Git repository or `git rev-parse` fails.
pub(crate) fn git_head_sha(root: &std::path::Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if sha.is_empty() {
        None
    } else {
        Some(sha)
    }
}

/// Stable hash of the scanned file tree: FNV-1a (64-bit, self-contained — no
/// hashing dependency) over the sorted `path / size / LOC` records, rendered
/// as 16 lowercase hex chars. Paths are normalised relative to `root` so the
/// hash describes the tree itself, not where it happens to be checked out; a
/// file whose metadata is unreadable contributes size 0.
pub(crate) fn tree_hash(entries: &[FileEntry], root: &std::path::Path) -> String {
    let mut records: Vec<(String, u64, u64)> = entries
        .iter()
        .map(|e| {
            let rel = std::path::Path::new(&e.path)
                .strip_prefix(root)
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_else(|_| e.path.clone());
            let size = std::fs::metadata(&e.path).map(|m| m.len()).unwrap_or(0);
            (rel, size, e.loc as u64)
        })
        .collect();
    records.sort();

    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = FNV_OFFSET;
    let mut feed = |bytes: &[u8]| {
        for &b in bytes {
            hash ^= u64::from(b);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
    };
    for (path, size, loc) in &records {
        feed(path.as_bytes());
        feed(&[0]);
        feed(&size.to_le_bytes());
        feed(&loc.to_le_bytes());
    }
    format!("{hash:016x}")
}

/// Model identifier for provenance: the `provider/model` pair(s) that scored
/// this run, or `"local-only"` when no LLM was involved.
pub(crate) fn model_label(llm_configs: &[LLMConfig]) -> String {
    if llm_configs.is_empty() {
        "local-only".to_string()
    } else {
        llm_configs
            .iter()
            .map(|c| format!("{}/{}", c.provider, c.model))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Assemble the provenance record for a review run.
pub(crate) fn build_metadata(
    local_path: &str,
    entries: &[FileEntry],
    llm_configs: &[LLMConfig],
    config: Option<&AppConfig>,
) -> ReviewMetadata {
    let root = std::path::Path::new(local_path);
    ReviewMetadata {
        head_sha: git_head_sha(root),
        tree_hash: tree_hash(entries, root),
        reviewed_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        model: model_label(llm_configs),
        // The same effective sampling parameter the LLM experts resolve
        // (config value, floored at 1), so a report records whether `score`
        // is a single evaluation or a sample median.
        score_samples: config.map(|c| c.scoring.score_samples).unwrap_or(1).max(1),
        scan_source: format!("local workspace on disk ({local_path})"),
    }
}
