use crate::models::*;

/// Output from the repo-review command.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RepoReviewOutput {
    pub overview: ReportOverview,
    pub expert_scores: Vec<ExpertScoreOutput>,
    pub risk_categories: Vec<RiskCategory>,
    pub action_items: Vec<ActionItem>,
    pub conclusion: ReportConclusion,
    /// Code-quality findings dropped by the optional verification pass, with
    /// reasons. Empty when the pass was disabled or kept everything.
    #[serde(default)]
    pub dropped_findings: Vec<crate::team::verifier::DroppedFinding>,
    /// Whether the verification pass actually executed during this review.
    /// `false` when the pass was disabled, when there were no code_quality
    /// findings to verify, or on the local-only path. Drives the honest
    /// "ran / skipped" wording in the Markdown appendix.
    #[serde(default)]
    pub verification_ran: bool,
    /// Coverage of the per-chunk code-quality pass: how many chunks were
    /// reviewed, how many fell back, and the per-failure breakdown (M26).
    /// `chunk_summary.total == 0` for the local-only path — the static
    /// experts don't chunk. Consumers that never chunk can ignore the
    /// field; reports render a `Chunks Reviewed` appendix only when at
    /// least one chunk was scheduled.
    #[serde(default)]
    pub chunk_summary: ChunkSummary,
    /// Provenance of the run that produced this report: what was scanned
    /// (git SHA / tree hash / source), when, and with which model. Lets a
    /// consumer trace a report back to the exact snapshot it describes and
    /// decide whether two reports are comparable before contrasting scores.
    /// `#[serde(default)]` keeps JSON written before the field existed
    /// deserializable.
    #[serde(default)]
    pub metadata: ReviewMetadata,
}

/// Per-chunk coverage of the code-quality pass (M26).
///
/// The chunk loop runs once per `CodeChunk` and records an
/// `ExpertScore` per chunk, but the JSON shape — one row per `ExpertScore`
/// with the SAME `expert_name = "code_quality"` — hides the per-chunk
/// signal behind an area-level aggregate. This summary captures the
/// module + LOC for every chunk and the reason it wasn't reviewed when
/// applicable, so a report can say "X of Y chunks were not assessed by
/// the model" instead of forcing the reader to grep the per-expert
/// `fallback: true` rows.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ChunkSummary {
    /// Total chunks scheduled for code-quality review (the Pass 2 input
    /// size). Zero on the local-only path.
    pub total: usize,
    /// Number of chunks that produced a real, non-fallback assessment.
    pub reviewed: usize,
    /// Number of chunks that landed on `chunk_fallback_score`: the LLM
    /// call failed or returned an empty completion that even the M26
    /// `disable_thinking` retry could not recover.
    pub not_reviewed: usize,
    /// Per-chunk detail for the not-reviewed set, in the order the loop
    /// scheduled them. `module` is the chunk's two-level directory key
    /// (e.g. `src/llm`); `loc` is the chunk's real evaluated LOC; `reason`
    /// is the verbatim error message recorded on the fallback score.
    pub failures: Vec<ChunkFailure>,
}

/// One chunk that did not reach a genuine assessment.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ChunkFailure {
    /// Module key the chunk scanner assigned (e.g. `src/llm`).
    pub module: String,
    /// Real LOC of the chunk — the same number
    /// [`crate::repo::experts::ExpertScore::evaluated_loc`] carries, so the
    /// report can quantify the LOC the model never saw.
    pub loc: u64,
    /// Verbatim error recorded on the fallback score (e.g.
    /// `"provider 'mimo-v2.5-pro' (model 'mimo-v2.5-pro') returned an empty completion ..."`).
    pub reason: String,
    /// M38: the [`crate::repo::experts::llm_experts::scoring::ParseReasonCode`]
    /// that classified the schema failure, when the chunk failure was a
    /// schema-broken response (missing keys / unparseable YAML). The same
    /// stable lowercase string the parser already logs at INFO — kept
    /// here so a reader can `grep reason_code=missing_expected_keys`
    /// across the rendered Markdown / JSON without re-parsing the prose
    /// `reason`. `None` for failures the parser never saw (LLM call
    /// transport errors, the M26 `disable_thinking` retry exhausting).
    /// `#[serde(default)]` keeps JSON written before the field existed
    /// deserializable.
    #[serde(default)]
    pub reason_code: Option<String>,
}

/// Provenance metadata for a repo-review report.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReviewMetadata {
    /// Git HEAD commit SHA of the scanned worktree; `None` when the scanned
    /// path is not a Git repository.
    pub head_sha: Option<String>,
    /// Stable hash of the scanned file tree: FNV-1a (64-bit, self-contained —
    /// no hashing dependency) over the sorted path+size+LOC records, rendered
    /// as 16 lowercase hex chars. Identical scan input always yields the same
    /// hash; adding / removing a file or changing a file's size or LOC
    /// changes it.
    pub tree_hash: String,
    /// RFC 3339 (UTC) timestamp of when the review ran.
    pub reviewed_at: String,
    /// Model identifier: comma-separated `provider/model` pairs when LLM
    /// experts ran, `"local-only"` on the static-only path.
    pub model: String,
    /// Effective `scoring.score_samples` sampling parameter for this run
    /// (1 = sampling disabled, each expert scored once).
    pub score_samples: usize,
    /// One-line description of what was scanned (the local workspace on
    /// disk).
    pub scan_source: String,
}

impl Default for ReviewMetadata {
    fn default() -> Self {
        Self {
            head_sha: None,
            tree_hash: String::new(),
            reviewed_at: String::new(),
            model: "local-only".to_string(),
            score_samples: 1,
            scan_source: String::new(),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReportOverview {
    pub health_score: u8,
    /// Unified risk level; serialized lowercase to keep the JSON contract
    /// (e.g. `"healthy"`, `"medium"`).
    #[serde(with = "crate::models::risk_level_lowercase")]
    pub risk_level: RiskLevel,
    pub total_experts: usize,
    pub total_files: usize,
    pub total_loc: usize,
    pub languages: Vec<String>,
    pub lead_summary: Option<String>,
    pub score_breakdown: Vec<ScoreRow>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ScoreRow {
    pub area: String,
    pub score: u8,
    pub weight: u8,
    pub weighted_contrib: f64,
    /// Unified risk level; serialized lowercase (see [`ReportOverview::risk_level`]).
    #[serde(with = "crate::models::risk_level_lowercase")]
    pub risk_label: RiskLevel,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RiskCategory {
    pub area: String,
    pub score: u8,
    /// Unified risk level; serialized lowercase (see [`ReportOverview::risk_level`]).
    #[serde(with = "crate::models::risk_level_lowercase")]
    pub risk_level: RiskLevel,
    pub finding_count: usize,
    pub findings: Vec<ScoreItemDetail>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ActionItem {
    pub area: String,
    pub severity: String,
    pub message: String,
    pub file: Option<String>,
    pub recommendation: String,
    pub effort: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReportConclusion {
    pub aggregated_score: u8,
    /// Unified risk level; serialized lowercase (see [`ReportOverview::risk_level`]).
    #[serde(with = "crate::models::risk_level_lowercase")]
    pub risk_level: RiskLevel,
    pub top_risks: Vec<(String, u8)>,
    pub recommendation: String,
}

/// A single finding rendered in the report output.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ScoreItemDetail {
    pub severity: String,
    pub message: String,
    pub file: Option<String>,
    pub evidence: Option<String>,
    pub impact: Option<String>,
    pub recommendation: Option<String>,
    pub effort: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExpertScoreOutput {
    pub name: String,
    pub weight: u8,
    pub score: u8,
    pub summary: String,
    pub details: Vec<ScoreItemDetail>,
    /// `true` when `score` is an explicit fallback (LLM call failed after all
    /// retries, the response was unparseable, or a static expert errored) —
    /// not a genuine assessment. Fallback experts still occupy their weight
    /// in the total, so consumers need this flag to interpret the score.
    #[serde(default)]
    pub fallback: bool,
    /// Raw per-sample scores when `scoring.score_samples > 1` was active for
    /// this expert; `score` is their median. Absent when sampling was
    /// disabled (the default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub samples: Option<Vec<u8>>,
    /// Smallest sample score; present exactly when `samples` is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_min: Option<u8>,
    /// Largest sample score; present exactly when `samples` is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_max: Option<u8>,
    /// M18-W2: how many scoring samples were attempted for this expert
    /// (including failures). Present exactly when `samples` is — the
    /// by-expert fallback rate is `samples_failed / samples_attempted`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub samples_attempted: Option<usize>,
    /// M18-W2: how many of the attempted scoring samples failed (call
    /// error, unparseable YAML, or schema-conforming YAML with no `score`).
    /// Present exactly when `samples` is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub samples_failed: Option<usize>,
}
