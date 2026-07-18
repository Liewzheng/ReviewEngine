//! Finding feedback storage: user verdicts on review findings.
//!
//! Users mark individual review findings as `useful` or `false_positive`.
//! Each finding is identified by a stable [`fingerprint`] derived from
//! `(file, line, title, category)`, so the same issue can be recognised
//! across repeated reviews. Verdicts are persisted as JSON to
//! `~/.config/review-engine/feedback.json` (override with the
//! `REVIEW_FEEDBACK_PATH` environment variable) using an atomic
//! temp-file-then-rename write, and cached in-process behind a `Mutex`.
//!
//! This module lives at the crate root so both the server (feedback API,
//! re-exported as `server::feedback`) and the review pipeline share the
//! exact same fingerprint algorithm and storage format. The pipeline side
//! is read-only: [`load_false_positive_fingerprints`] loads the set of
//! fingerprints the user marked as false positives so the orchestrator can
//! filter those findings out of subsequent reviews.
//!
//! The aggregated [`FeedbackStats`] (hit rate / false-positive rate, also
//! grouped by category) provide the data basis for later prompt
//! calibration and false-positive reduction (see
//! `docs/professional_team_design.md` §6.3 / §8.9).

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Environment variable overriding the default feedback file location.
pub const FEEDBACK_PATH_ENV: &str = "REVIEW_FEEDBACK_PATH";

/// Bucket label used in stats for feedback recorded without a category.
pub const UNKNOWN_CATEGORY: &str = "unknown";

/// User verdict on a single finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// The finding was correct and helpful.
    Useful,
    /// The finding was a false positive.
    FalsePositive,
}

/// One feedback record for a finding identified by its stable fingerprint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FindingFeedback {
    /// Stable fingerprint of the finding (see [`fingerprint`]).
    pub finding_fingerprint: String,
    /// User verdict: useful or false positive.
    pub verdict: Verdict,
    /// Optional free-text comment from the user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    /// Finding category, denormalised when known so stats can be grouped
    /// per category. Absent for feedback submitted by fingerprint only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// When the feedback was recorded.
    pub created_at: DateTime<Utc>,
}

/// Aggregated feedback statistics for one category (or overall).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CategoryStats {
    /// Total number of feedback records.
    pub total: u64,
    /// Number of `useful` verdicts.
    pub useful: u64,
    /// Number of `false_positive` verdicts.
    pub false_positive: u64,
    /// `false_positive / total` (0.0 when there is no feedback).
    pub false_positive_rate: f64,
}

/// Aggregated feedback statistics: totals plus a per-category breakdown.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FeedbackStats {
    /// Total number of feedback records.
    pub total: u64,
    /// Number of `useful` verdicts.
    pub useful: u64,
    /// Number of `false_positive` verdicts.
    pub false_positive: u64,
    /// `false_positive / total` (0.0 when there is no feedback).
    pub false_positive_rate: f64,
    /// Per-category statistics, keyed by category name. Feedback recorded
    /// without a category is grouped under `"unknown"`.
    pub by_category: BTreeMap<String, CategoryStats>,
}

/// Compute a stable fingerprint for a finding from its identity fields.
///
/// SHA-256 over `(file, line, title, category)` — fields separated by a
/// `0x1f` unit separator so field boundaries are unambiguous — truncated
/// to the first 16 hex characters (64 bits). A `None` line hashes as an
/// empty field. The fingerprint is stable across reviews as long as the
/// finding keeps the same file, line, title and category.
pub fn fingerprint(file: &str, line: Option<u32>, title: &str, category: &str) -> String {
    let line_str = line.map(|l| l.to_string()).unwrap_or_default();
    let mut hasher = Sha256::new();
    for part in [file, line_str.as_str(), title, category] {
        hasher.update(part.as_bytes());
        hasher.update([0x1f]);
    }
    hex::encode(&hasher.finalize()[..8])
}

/// Load the set of finding fingerprints the user marked `false_positive`
/// from the default feedback file (`REVIEW_FEEDBACK_PATH` if set, otherwise
/// `~/.config/review-engine/feedback.json`).
///
/// Read-only and fail-open: a missing or corrupt file yields an empty set
/// (with a warn log for the corrupt case), so the review pipeline is never
/// blocked by feedback storage problems.
pub fn load_false_positive_fingerprints() -> HashSet<String> {
    match default_path() {
        Some(path) => load_false_positive_fingerprints_from(&path),
        None => HashSet::new(),
    }
}

/// [`load_false_positive_fingerprints`] with an explicit file path.
///
/// Used by tests and by callers that resolve the feedback location
/// themselves. Same fail-open semantics as the default-path variant.
pub fn load_false_positive_fingerprints_from(path: &Path) -> HashSet<String> {
    load_entries(path)
        .into_iter()
        .filter(|entry| entry.verdict == Verdict::FalsePositive)
        .map(|entry| entry.finding_fingerprint)
        .collect()
}

/// JSON-file-backed feedback store with an in-process cache.
///
/// All records are held in memory; every [`record`](Self::record) call
/// rewrites the JSON file atomically (temp file + rename) so a crash
/// mid-write never leaves a truncated file behind.
#[derive(Debug)]
pub struct FeedbackStore {
    entries: Mutex<Vec<FindingFeedback>>,
    path: Option<PathBuf>,
}

impl FeedbackStore {
    /// Store backed by the default feedback file:
    /// `REVIEW_FEEDBACK_PATH` if set, otherwise
    /// `~/.config/review-engine/feedback.json`.
    pub fn persistent() -> Self {
        Self::with_path(default_path())
    }

    /// Explicit constructor: `path == None` keeps feedback in memory only.
    /// Existing records are loaded from disk; a missing or corrupt file
    /// yields an empty store (with a warn log for the corrupt case).
    pub fn with_path(path: Option<PathBuf>) -> Self {
        let entries = path.as_deref().map(load_entries).unwrap_or_default();
        Self {
            entries: Mutex::new(entries),
            path,
        }
    }

    /// Append a feedback record and persist the full log atomically.
    pub fn record(&self, feedback: FindingFeedback) -> std::io::Result<()> {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries.push(feedback);
        if let Some(path) = &self.path {
            write_entries_atomic(path, &entries)?;
        }
        Ok(())
    }

    /// Snapshot of all recorded feedback, oldest first.
    pub fn entries(&self) -> Vec<FindingFeedback> {
        self.entries.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Aggregate current feedback into totals and per-category statistics.
    pub fn stats(&self) -> FeedbackStats {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let mut useful = 0u64;
        let mut false_positive = 0u64;
        let mut by_category: BTreeMap<String, CategoryStats> = BTreeMap::new();
        for entry in entries.iter() {
            let category = entry.category.as_deref().unwrap_or(UNKNOWN_CATEGORY).to_string();
            let cat = by_category.entry(category).or_insert(CategoryStats {
                total: 0,
                useful: 0,
                false_positive: 0,
                false_positive_rate: 0.0,
            });
            cat.total += 1;
            match entry.verdict {
                Verdict::Useful => {
                    useful += 1;
                    cat.useful += 1;
                }
                Verdict::FalsePositive => {
                    false_positive += 1;
                    cat.false_positive += 1;
                }
            }
        }
        for cat in by_category.values_mut() {
            cat.false_positive_rate = rate(cat.false_positive, cat.total);
        }
        let total = entries.len() as u64;
        FeedbackStats {
            total,
            useful,
            false_positive,
            false_positive_rate: rate(false_positive, total),
            by_category,
        }
    }
}

/// `part / total` as a rate, defined as 0.0 when `total == 0`.
fn rate(part: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        part as f64 / total as f64
    }
}

/// Feedback file location: `REVIEW_FEEDBACK_PATH` or the default config path.
fn default_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var(FEEDBACK_PATH_ENV) {
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }
    home::home_dir().map(|dir| dir.join(".config").join("review-engine").join("feedback.json"))
}

/// Load feedback records from disk. Missing files yield an empty log;
/// corrupt files are ignored with a warn log.
fn load_entries(path: &Path) -> Vec<FindingFeedback> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            tracing::warn!("Feedback: failed to read {}: {e}", path.display());
            return Vec::new();
        }
    };
    match serde_json::from_str(&content) {
        Ok(entries) => entries,
        Err(e) => {
            tracing::warn!("Feedback: ignoring corrupt file {}: {e}", path.display());
            Vec::new()
        }
    }
}

/// Serialize `entries` to `path` atomically via a temp file + rename, so a
/// crash mid-write never leaves a truncated feedback file behind.
fn write_entries_atomic(path: &Path, entries: &[FindingFeedback]) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(entries).map_err(std::io::Error::other)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp_path = path.with_extension("tmp");
    std::fs::write(&tmp_path, json)?;
    std::fs::rename(&tmp_path, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_feedback(fingerprint: &str, verdict: Verdict, category: Option<&str>) -> FindingFeedback {
        FindingFeedback {
            finding_fingerprint: fingerprint.to_string(),
            verdict,
            comment: None,
            category: category.map(str::to_string),
            created_at: Utc::now(),
        }
    }

    // ─── fingerprint ─────────────────────────────

    #[test]
    fn test_fingerprint_is_stable() {
        let a = fingerprint("src/main.rs", Some(42), "SQL injection", "security");
        let b = fingerprint("src/main.rs", Some(42), "SQL injection", "security");
        assert_eq!(a, b);
        assert_eq!(a.len(), 16);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_fingerprint_differs_per_field() {
        let base = fingerprint("src/main.rs", Some(42), "SQL injection", "security");
        assert_ne!(base, fingerprint("src/other.rs", Some(42), "SQL injection", "security"));
        assert_ne!(base, fingerprint("src/main.rs", Some(43), "SQL injection", "security"));
        assert_ne!(base, fingerprint("src/main.rs", None, "SQL injection", "security"));
        assert_ne!(base, fingerprint("src/main.rs", Some(42), "XSS", "security"));
        assert_ne!(base, fingerprint("src/main.rs", Some(42), "SQL injection", "quality"));
    }

    #[test]
    fn test_fingerprint_field_boundaries_are_unambiguous() {
        // Without a field separator these two would hash identically.
        let a = fingerprint("ab", None, "c", "d");
        let b = fingerprint("a", None, "bc", "d");
        assert_ne!(a, b);
    }

    // ─── read-only false-positive loading ────────

    #[test]
    fn test_load_false_positive_fingerprints_from_keeps_only_false_positives() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("feedback.json");

        let store = FeedbackStore::with_path(Some(path.clone()));
        store
            .record(sample_feedback("fp-useful", Verdict::Useful, None))
            .unwrap();
        store
            .record(sample_feedback("fp-bad-1", Verdict::FalsePositive, None))
            .unwrap();
        store
            .record(sample_feedback("fp-bad-2", Verdict::FalsePositive, None))
            .unwrap();
        drop(store);

        let set = load_false_positive_fingerprints_from(&path);
        assert_eq!(set.len(), 2);
        assert!(set.contains("fp-bad-1"));
        assert!(set.contains("fp-bad-2"));
        assert!(!set.contains("fp-useful"));
    }

    #[test]
    fn test_load_false_positive_fingerprints_from_missing_file_yields_empty_set() {
        let dir = tempfile::tempdir().unwrap();
        let set = load_false_positive_fingerprints_from(&dir.path().join("does-not-exist.json"));
        assert!(set.is_empty());
    }

    #[test]
    fn test_load_false_positive_fingerprints_from_corrupt_file_yields_empty_set() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("feedback.json");
        std::fs::write(&path, "{ not valid json").unwrap();
        let set = load_false_positive_fingerprints_from(&path);
        assert!(set.is_empty());
    }

    // ─── storage ─────────────────────────────────

    #[test]
    fn test_record_and_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("feedback.json");

        let store = FeedbackStore::with_path(Some(path.clone()));
        store
            .record(sample_feedback("fp1", Verdict::Useful, Some("security")))
            .unwrap();
        store
            .record(sample_feedback("fp2", Verdict::FalsePositive, Some("quality")))
            .unwrap();
        drop(store);

        let reloaded = FeedbackStore::with_path(Some(path));
        let entries = reloaded.entries();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].finding_fingerprint, "fp1");
        assert_eq!(entries[0].verdict, Verdict::Useful);
        assert_eq!(entries[1].finding_fingerprint, "fp2");
        assert_eq!(entries[1].verdict, Verdict::FalsePositive);
    }

    #[test]
    fn test_write_is_atomic_and_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("feedback.json");

        let store = FeedbackStore::with_path(Some(path.clone()));
        store.record(sample_feedback("fp1", Verdict::Useful, None)).unwrap();

        // The temp file must not survive the atomic rename.
        assert!(!path.with_extension("tmp").exists());
        // The target must contain a valid JSON array with the record.
        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: Vec<FindingFeedback> = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].finding_fingerprint, "fp1");
    }

    #[test]
    fn test_missing_file_yields_empty_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = FeedbackStore::with_path(Some(dir.path().join("does-not-exist.json")));
        assert!(store.entries().is_empty());
    }

    #[test]
    fn test_corrupt_file_yields_empty_store() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("feedback.json");
        std::fs::write(&path, "{ not valid json").unwrap();
        let store = FeedbackStore::with_path(Some(path));
        assert!(store.entries().is_empty());
    }

    #[test]
    fn test_in_memory_store_keeps_entries_without_writing() {
        let store = FeedbackStore::with_path(None);
        store.record(sample_feedback("fp1", Verdict::Useful, None)).unwrap();
        assert_eq!(store.entries().len(), 1);
    }

    #[test]
    fn test_default_path_honours_env_override() {
        let dir = tempfile::tempdir().unwrap();
        let custom = dir.path().join("custom-feedback.json");
        std::env::set_var(FEEDBACK_PATH_ENV, &custom);
        assert_eq!(default_path(), Some(custom));
        std::env::remove_var(FEEDBACK_PATH_ENV);
    }

    // ─── stats ───────────────────────────────────

    #[test]
    fn test_stats_empty_store() {
        let store = FeedbackStore::with_path(None);
        let stats = store.stats();
        assert_eq!(stats.total, 0);
        assert_eq!(stats.useful, 0);
        assert_eq!(stats.false_positive, 0);
        assert_eq!(stats.false_positive_rate, 0.0);
        assert!(stats.by_category.is_empty());
    }

    #[test]
    fn test_stats_counts_and_rates() {
        let store = FeedbackStore::with_path(None);
        store
            .record(sample_feedback("fp1", Verdict::Useful, Some("security")))
            .unwrap();
        store
            .record(sample_feedback("fp2", Verdict::FalsePositive, Some("security")))
            .unwrap();
        store
            .record(sample_feedback("fp3", Verdict::Useful, Some("quality")))
            .unwrap();
        store
            .record(sample_feedback("fp4", Verdict::FalsePositive, None))
            .unwrap();

        let stats = store.stats();
        assert_eq!(stats.total, 4);
        assert_eq!(stats.useful, 2);
        assert_eq!(stats.false_positive, 2);
        assert!((stats.false_positive_rate - 0.5).abs() < f64::EPSILON);

        let security = &stats.by_category["security"];
        assert_eq!(security.total, 2);
        assert_eq!(security.useful, 1);
        assert_eq!(security.false_positive, 1);
        assert!((security.false_positive_rate - 0.5).abs() < f64::EPSILON);

        let quality = &stats.by_category["quality"];
        assert_eq!(quality.total, 1);
        assert_eq!(quality.useful, 1);
        assert_eq!(quality.false_positive_rate, 0.0);

        // Fingerprint-only feedback (no category) lands in "unknown".
        let unknown = &stats.by_category[UNKNOWN_CATEGORY];
        assert_eq!(unknown.total, 1);
        assert_eq!(unknown.false_positive, 1);
        assert!((unknown.false_positive_rate - 1.0).abs() < f64::EPSILON);
    }
}
