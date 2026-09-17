//! REST API endpoints for the dashboard overview page.
//!
//! Aggregates KPIs, 24h trend, 14-day daily trend, system health, and recent
//! reviews.
//!
//! RENG-32: every review-derived figure (KPIs / 24h trend / recentReviews)
//! is aggregated from the persistent `ReviewStore` (0.10.0, SQLite/PG) —
//! the in-memory task store's reaper deletes completed entries 30 minutes
//! after completion, which left the dashboard at zero on a production box
//! with 100+ rows in `reviews`. The in-memory store remains the source for
//! `activeQueue` (live queue state — that source is correct) and for the
//! whole payload when persistence is disabled (db=None: `REVIEW_DISABLE_DB=1`,
//! unit tests), mirroring the `/reviews` handler's DB-first/in-memory
//! fallback split.
//!
//! Window semantics: "this week" is the current ISO week (Monday 00:00) and
//! "today" / "yesterday" are calendar days, both in the server's LOCAL
//! timezone (`chrono::Local`; containers should pin `TZ` to the intended
//! zone — otherwise the image default, usually UTC, applies). Trend deltas
//! are `null` when the comparison window has no data; the KPI cards render
//! "—" instead of a fabricated 0%. `successRate` / `avgDurationMs` are
//! likewise `null` when the current week has no reviews to compute them
//! from; `successTrend` is a percentage-POINT delta vs yesterday (a rate
//! comparison in relative percent would explode near 0%), while
//! `reviewsTrend` / `durationTrend` are relative percent changes vs last
//! week.
//!
//! Cost: the frontend polls every 60 s. Counts are SQL `COUNT(*)` via
//! `list_reviews`' total (per_page=1, nothing materialized); only the two
//! duration averages, the trend bucketing (one shared window fetch feeds both
//! the 24h and the 14-day daily series), and recentReviews materialize rows —
//! each capped at [`WINDOW_ROW_CAP`] of the newest rows in its window (a few
//! hundred rows per poll, indexed `created_at` range scans).

use axum::{extract::State, http::StatusCode, response::IntoResponse, routing::get, Json, Router};
use std::sync::Arc;

use crate::server::api::llm_health::{ProviderHealth, ProviderStatus};
use crate::server::task_queue::{TaskEntry, TaskState};
use crate::server::AppState;
use crate::store::traits::{ReviewListQuery, ReviewStore};
use chrono::{DateTime, Datelike, Local, NaiveDate, Utc};

/// Row cap for windows that materialize rows (duration averages, 24h
/// buckets). Counts never materialize (they read `list_reviews`' total);
/// these degrade to "the newest ≤500 rows of the window" in unusually
/// heavy weeks instead of loading the table unboundedly.
const WINDOW_ROW_CAP: u64 = 500;

/// Recent-reviews card size (latest N, `created_at` DESC).
const RECENT_REVIEWS_LIMIT: u64 = 5;

/// Daily-trend length: one point per calendar day for the last 14 local days
/// INCLUDING today (today is the final, partial day).
const TREND_DAILY_DAYS: i64 = 14;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/", get(get_dashboard))
}

/// Review rows source: the persistent DB (production) or the in-memory task
/// store (db=None fallback). Both expose the same windowed list shape
/// (inclusive `created_at` bounds + newest-first page), so the aggregation
/// below is source-agnostic.
enum ReviewSource<'a> {
    Db(&'a Arc<crate::store::SqlxStore>),
    Memory(&'a crate::server::task_queue::TaskStore),
}

impl ReviewSource<'_> {
    /// Windowed page: `status`/`[from, to]` filter on `created_at`,
    /// newest first. Returns `(rows, total)`; totals are exact SQL counts
    /// on the DB path regardless of `per_page`.
    async fn window(
        &self,
        status: Option<TaskState>,
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
        per_page: u64,
    ) -> anyhow::Result<(Vec<TaskEntry>, u64)> {
        match self {
            Self::Db(db) => {
                db.list_reviews(&ReviewListQuery {
                    status,
                    page: 1,
                    per_page,
                    date_from: from,
                    date_to: to,
                    ..Default::default()
                })
                .await
            }
            Self::Memory(store) => Ok(store.list(status, 1, per_page, None, None, None, from, to).await),
        }
    }
}

async fn get_dashboard(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let now_local = Local::now();
    let week_start = iso_week_start(now_local);
    let today_start = day_start(now_local);

    let health = compute_health(&state).await;

    // activeQueue is LIVE queue state: pending + running right now. The
    // in-memory store is the correct (and only) source for this — the DB
    // would include long-finished history rows.
    let active_queue = match &state.task_store {
        Some(store) => {
            let (items, _) = store.list(None, 1, 1000, None, None, None, None, None).await;
            items
                .iter()
                .filter(|e| e.state == TaskState::Running || e.state == TaskState::Pending)
                .count() as u64
        }
        None => 0,
    };

    // Review-derived payload: DB when persistence is active; the in-memory
    // store when db=None (REVIEW_DISABLE_DB=1, unit tests) — same split the
    // /reviews handler makes. With neither store, serve documented defaults.
    let source = match (&state.db, &state.task_store) {
        (Some(db), _) => ReviewSource::Db(db),
        (None, Some(store)) => ReviewSource::Memory(store),
        (None, None) => {
            return Json(serde_json::json!({
                "kpis": default_kpis(),
                "trend": default_trend(),
                "trendDaily": default_trend_daily(),
                "health": health,
                "recentReviews": [],
            }))
            .into_response()
        }
    };

    let collected = collect_dashboard(source, now_local, week_start, today_start, active_queue).await;
    let (kpis, trend, trend_daily, recent_reviews) = match collected {
        Ok(payload) => payload,
        Err(e) => {
            // Same policy as the /reviews handler: a failed history read is
            // a 500, not a page of fabricated zeros.
            tracing::error!("failed to aggregate dashboard data: {e:#}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": "failed to load dashboard data" })),
            )
                .into_response();
        }
    };

    Json(serde_json::json!({
        "kpis": kpis,
        "trend": trend,
        "trendDaily": trend_daily,
        "health": health,
        "recentReviews": recent_reviews,
    }))
    .into_response()
}

/// Raw window counts feeding the KPI cards.
#[derive(Default)]
struct WindowCounts {
    /// All reviews created in the current ISO week (`reviewsThisWeek`).
    this_week_all: u64,
    /// All reviews created in the previous ISO week (WoW comparison).
    last_week_all: u64,
    this_week_completed: u64,
    this_week_failed: u64,
    today_completed: u64,
    today_failed: u64,
    yesterday_completed: u64,
    yesterday_failed: u64,
    /// Average duration of completed reviews this week / last week
    /// (`None` = no completed review in the window).
    this_week_avg_ms: Option<u64>,
    last_week_avg_ms: Option<u64>,
}

/// Runs the dashboard queries against one source and assembles the
/// kpis/trend/trendDaily/recentReviews payload. `now_local` is the anchor all
/// local-time windows are computed from; `week_start` / `today_start` are its
/// precomputed ISO-week / day anchors.
async fn collect_dashboard(
    source: ReviewSource<'_>,
    now_local: DateTime<Local>,
    week_start: DateTime<Utc>,
    today_start: DateTime<Utc>,
    active_queue: u64,
) -> anyhow::Result<(
    serde_json::Value,
    Vec<serde_json::Value>,
    Vec<serde_json::Value>,
    Vec<serde_json::Value>,
)> {
    // The store filters are inclusive on both ends; shave 1µs off each open
    // end so adjacent windows (yesterday/today, last week/this week) never
    // double-count a row created exactly on the boundary.
    let yesterday_start = today_start - chrono::Duration::days(1);
    let yesterday_end = today_start - chrono::Duration::microseconds(1);
    let last_week_start = week_start - chrono::Duration::days(7);
    let last_week_end = week_start - chrono::Duration::microseconds(1);
    // First day covered by the daily trend (local midnight 13 days ago —
    // local_midnight, not `today_start - 13d`: across a DST transition a
    // local day is not 86400 s, so fixed-second arithmetic would drift an
    // hour off the calendar-day anchor).
    let trend_window_start = local_midnight(
        now_local.date_naive() - chrono::Duration::days(TREND_DAILY_DAYS - 1),
        now_local,
    );

    // ── Counts (per_page=1: only the exact SQL COUNT total is used) ──
    let this_week_all = source.window(None, Some(week_start), None, 1).await?.1;
    let last_week_all = source
        .window(None, Some(last_week_start), Some(last_week_end), 1)
        .await?
        .1;
    let this_week_completed = source
        .window(Some(TaskState::Completed), Some(week_start), None, 1)
        .await?
        .1;
    let this_week_failed = source
        .window(Some(TaskState::Failed), Some(week_start), None, 1)
        .await?
        .1;
    let today_completed = source
        .window(Some(TaskState::Completed), Some(today_start), None, 1)
        .await?
        .1;
    let today_failed = source
        .window(Some(TaskState::Failed), Some(today_start), None, 1)
        .await?
        .1;
    let yesterday_completed = source
        .window(
            Some(TaskState::Completed),
            Some(yesterday_start),
            Some(yesterday_end),
            1,
        )
        .await?
        .1;
    let yesterday_failed = source
        .window(Some(TaskState::Failed), Some(yesterday_start), Some(yesterday_end), 1)
        .await?
        .1;

    // ── Row-materializing windows (capped, newest first) ──
    let (this_week_completed_rows, _) = source
        .window(Some(TaskState::Completed), Some(week_start), None, WINDOW_ROW_CAP)
        .await?;
    let (last_week_completed_rows, _) = source
        .window(
            Some(TaskState::Completed),
            Some(last_week_start),
            Some(last_week_end),
            WINDOW_ROW_CAP,
        )
        .await?;

    let counts = WindowCounts {
        this_week_all,
        last_week_all,
        this_week_completed,
        this_week_failed,
        today_completed,
        today_failed,
        yesterday_completed,
        yesterday_failed,
        this_week_avg_ms: avg_duration_ms(&this_week_completed_rows),
        last_week_avg_ms: avg_duration_ms(&last_week_completed_rows),
    };
    let kpis = compute_kpis(&counts, active_queue);

    // One shared bounded fetch feeds BOTH trend series: the 24h rolling
    // buckets and the 14-day daily buckets read the same newest-≤WINDOW_ROW_CAP
    // rows of [trend_window_start, now] (13 days ago trivially covers the
    // 24h window), so the 60s poll pays for a single window query instead of
    // two.
    let (trend_rows, _) = source
        .window(None, Some(trend_window_start), None, WINDOW_ROW_CAP)
        .await?;
    let trend = compute_trend(&trend_rows);
    let trend_daily = compute_trend_daily(&trend_rows, now_local);

    let (recent_rows, _) = source.window(None, None, None, RECENT_REVIEWS_LIMIT).await?;
    let recent_reviews = compute_recent_reviews(&recent_rows);

    Ok((kpis, trend, trend_daily, recent_reviews))
}

/// 00:00 local time on `date`, expressed as UTC. The conversion uses the
/// UTC offset of `offset_at` (the "now" the window is anchored to) — pure,
/// and never ambiguous the way a named-zone midnight resolution could be
/// on the two DST-transition days a year.
fn local_midnight(date: NaiveDate, offset_at: DateTime<Local>) -> DateTime<Utc> {
    // `NaiveTime::MIN` is midnight; `and_time` is total (no fallible opt).
    let midnight = date.and_time(chrono::NaiveTime::MIN);
    DateTime::<Utc>::from_naive_utc_and_offset(midnight - *offset_at.offset(), Utc)
}

/// Monday 00:00 local time of the ISO week (Monday first) containing `now`.
fn iso_week_start(now: DateTime<Local>) -> DateTime<Utc> {
    let today = now.date_naive();
    let days_since_monday = i64::from(today.weekday().num_days_from_monday());
    local_midnight(today - chrono::Duration::days(days_since_monday), now)
}

/// 00:00 local time of the calendar day containing `now`.
fn day_start(now: DateTime<Local>) -> DateTime<Utc> {
    local_midnight(now.date_naive(), now)
}

/// Relative percent change `(current - previous) / previous * 100`, rounded
/// to one decimal. `None` when the comparison window is empty — the KPI
/// card renders "—", never a fabricated 0.0%.
fn pct_change(current: u64, previous: u64) -> Option<f64> {
    if previous == 0 {
        return None;
    }
    Some(round1((current as f64 - previous as f64) / previous as f64 * 100.0))
}

/// Percentage-point delta between two rates (e.g. today's success rate vs
/// yesterday's). `None` when either window has no data — the rate there is
/// undefined, so any number would be fabricated.
fn rate_delta(current: Option<f64>, previous: Option<f64>) -> Option<f64> {
    Some(round1(current? - previous?))
}

/// Round to one decimal place.
fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

/// completed/(completed+failed) in percent; `None` when the window has no
/// terminal reviews.
fn success_rate(completed: u64, failed: u64) -> Option<f64> {
    let total = completed + failed;
    if total == 0 {
        None
    } else {
        Some(round1(completed as f64 * 100.0 / total as f64))
    }
}

/// Average duration of the completed rows; `None` when the window has no
/// completed review (or none with a measurable duration).
fn avg_duration_ms(rows: &[TaskEntry]) -> Option<u64> {
    let mut total = 0u64;
    let mut n = 0u64;
    for row in rows {
        if let Some(ms) = row.duration_ms() {
            total = total.saturating_add(ms);
            n += 1;
        }
    }
    (n > 0).then(|| total / n)
}

fn compute_kpis(w: &WindowCounts, active_queue: u64) -> serde_json::Value {
    serde_json::json!({
        "reviewsThisWeek": w.this_week_all,
        "reviewsTrend": pct_change(w.this_week_all, w.last_week_all),
        "activeQueue": active_queue,
        "successRate": success_rate(w.this_week_completed, w.this_week_failed),
        "successTrend": rate_delta(
            success_rate(w.today_completed, w.today_failed),
            success_rate(w.yesterday_completed, w.yesterday_failed),
        ),
        "avgDurationMs": w.this_week_avg_ms,
        "durationTrend": match (w.this_week_avg_ms, w.last_week_avg_ms) {
            (Some(current), Some(previous)) => pct_change(current, previous),
            _ => None,
        },
    })
}

fn compute_trend(items: &[TaskEntry]) -> Vec<serde_json::Value> {
    let now = chrono::Utc::now();
    let mut points = Vec::new();
    for i in (0..24).rev() {
        let hour_start = now - chrono::Duration::hours(i + 1);
        let hour_end = now - chrono::Duration::hours(i);
        let count = items
            .iter()
            .filter(|e| e.created_at >= hour_start && e.created_at < hour_end)
            .count() as u64;
        points.push(serde_json::json!({
            "time": hour_end.timestamp(),
            "value": count,
        }));
    }
    points
}

/// RENG-50: one point per calendar day for the last [`TREND_DAILY_DAYS`]
/// local days INCLUDING today (the final, partial day), ordered oldest →
/// newest like `compute_trend`. `time` is that day's local midnight as a unix
/// timestamp — the same `local_midnight` anchor math `day_start` uses, so the
/// daily series lines up exactly with the KPI "today" window — and `value`
/// counts reviews whose `created_at` falls in `[midnight, next midnight)`.
/// Each bucket's bounds go through `local_midnight` rather than fixed-second
/// arithmetic so DST-transition days (23 h / 25 h) still bucket by calendar
/// day.
fn compute_trend_daily(items: &[TaskEntry], now: DateTime<Local>) -> Vec<serde_json::Value> {
    let today = now.date_naive();
    let mut points = Vec::with_capacity(TREND_DAILY_DAYS as usize);
    for age in (0..TREND_DAILY_DAYS).rev() {
        let day = today - chrono::Duration::days(age);
        let day_start_ts = local_midnight(day, now);
        let day_end_ts = local_midnight(day + chrono::Duration::days(1), now);
        let count = items
            .iter()
            .filter(|e| e.created_at >= day_start_ts && e.created_at < day_end_ts)
            .count() as u64;
        points.push(serde_json::json!({
            "time": day_start_ts.timestamp(),
            "value": count,
        }));
    }
    points
}

async fn compute_health(state: &AppState) -> serde_json::Value {
    // RENG-97: the integration rows report the platform probe cache — the
    // Configuration page's `POST /config/git-platforms/test` is the only real
    // authenticated check, so it is the single source of truth the dashboard
    // and `GET /system/health` both read. Entry presence (a `git_platforms`
    // row, or the startup env/CLI token flags) is never health: an
    // unprobed-but-configured integration reads `unknown`, and a failed probe
    // reads `error` with the failure. The rows are built outside any guard
    // (each reads its own lock internally, never across an await).
    let integrations = vec![
        super::git_health::integration_row(state, "GitLab API", "gitlab", state.env_gitlab_configured),
        super::git_health::integration_row(state, "GitHub API", "github", state.env_github_configured),
    ];

    // LLM provider health: the probe cache (RENG-36) — one report per
    // configured provider, sharing `GET /api/v1/llm/providers`' source so the
    // dashboard and the LLM page cannot disagree. A provider whose credentials
    // were just changed has no entry for its new config, so it is probed before
    // this returns rather than reported from the pre-change status. The
    // `latencyMs` field stays dropped (RENG-32: the dashboard poll shows no
    // per-provider timing; `POST /api/v1/llm/providers/{id}/test` reports it).
    // Never hold the `llm_configs` guard across the probe `await`.
    let llm_configs: Vec<crate::models::LLMConfig> = state.llm_configs.read().unwrap().clone();
    let health = state.llm_health.report(&llm_configs).await;
    let mut llm_providers = Vec::new();
    for (llm, report) in llm_configs.iter().zip(health.iter()) {
        llm_providers.push(serde_json::json!({
            "service": format!("{} {}", llm.provider, llm.model),
            "type": "llm",
            "status": report.status.dashboard_str(),
            "message": report.message,
        }));
    }

    // `overall` follows the probed statuses of the ENABLED providers, not
    // provider presence: a configured-but-failing provider is no longer
    // "operational", while a disabled one (RENG-75) is deliberately off and
    // must not drag the panel to `warning`/`error`. With nothing enabled
    // (none configured, or every one switched off) the LLM subsystem is
    // simply not serving: `offline`, like the empty case.
    let enabled: Vec<&ProviderHealth> = health.iter().filter(|h| h.status != ProviderStatus::Disabled).collect();
    let overall = if enabled.is_empty() {
        "offline"
    } else if enabled.iter().all(|h| h.status == ProviderStatus::Healthy) {
        "success"
    } else if enabled.iter().any(|h| h.status == ProviderStatus::Healthy) {
        "warning"
    } else {
        "error"
    };

    serde_json::json!({
        "integrations": integrations,
        "llmProviders": llm_providers,
        "overall": overall,
        "lastChecked": chrono::Utc::now().to_rfc3339(),
    })
}

fn compute_recent_reviews(items: &[TaskEntry]) -> Vec<serde_json::Value> {
    // No state filter: all tasks (including pending/cancelled) surface here,
    // each with its real status vocabulary.
    let mut recent: Vec<&TaskEntry> = items.iter().collect();
    recent.sort_by_key(|b| std::cmp::Reverse(b.created_at));
    recent.truncate(RECENT_REVIEWS_LIMIT as usize);

    recent
        .iter()
        .map(|e| {
            let meta = &e.source_meta;
            serde_json::json!({
                "id": e.task_id.to_string(),
                // Absent values are `null`, consistent with `/reviews` (the
                // frontend applies its own display defaults).
                "mrTitle": meta.mr_title.clone(),
                "project": meta.project.clone(),
                "author": {
                    "name": meta.author_name.clone(),
                    "avatarUrl": meta.author_avatar_url.clone(),
                },
                // Real task state vocabulary, consistent with `/reviews`.
                "status": super::review::task_status_str(&e.state),
                "durationMs": e.duration_ms().unwrap_or(0),
                "createdAt": e.created_at.to_rfc3339(),
            })
        })
        .collect()
}

fn default_kpis() -> serde_json::Value {
    serde_json::json!({
        "reviewsThisWeek": 0,
        "reviewsTrend": null,
        "activeQueue": 0,
        "successRate": null,
        "successTrend": null,
        "avgDurationMs": null,
        "durationTrend": null,
    })
}

fn default_trend() -> Vec<serde_json::Value> {
    let now = chrono::Utc::now();
    (0..24)
        .rev()
        .map(|i| {
            serde_json::json!({
                "time": (now - chrono::Duration::hours(i)).timestamp(),
                "value": 0,
            })
        })
        .collect()
}

/// No-store fallback for the daily series: same shape as `compute_trend_daily`
/// on empty input ([`TREND_DAILY_DAYS`] zero points on the local-midnight
/// anchors).
fn default_trend_daily() -> Vec<serde_json::Value> {
    compute_trend_daily(&[], Local::now())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::api::llm_health::LlmHealthStore;
    use crate::server::task_queue::{SourceMeta, TaskStore};
    use crate::store::SqlxStore;
    use chrono::TimeZone;

    /// A store whose probe always succeeds (`ok`) or always fails — the seam
    /// that keeps the dashboard's health tests off the network (RENG-36).
    fn stub_health_store(ok: bool) -> LlmHealthStore {
        LlmHealthStore::with_probe(
            Arc::new(move |_cfg| {
                Box::pin(async move {
                    if ok {
                        Ok(crate::llm::probe::ProbeOutcome {
                            resolved_base: "stub".to_string(),
                        })
                    } else {
                        Err("HTTP 401 Unauthorized".to_string())
                    }
                })
            }),
            std::time::Duration::from_secs(60),
        )
    }

    /// A store whose verdict depends on the provider name (`openai` healthy,
    /// anything else 401) — deterministic whatever order the concurrent probes
    /// complete in.
    fn per_provider_health_store() -> LlmHealthStore {
        LlmHealthStore::with_probe(
            Arc::new(|cfg| {
                let ok = cfg.provider == "openai";
                Box::pin(async move {
                    if ok {
                        Ok(crate::llm::probe::ProbeOutcome {
                            resolved_base: "stub".to_string(),
                        })
                    } else {
                        Err("HTTP 401 Unauthorized".to_string())
                    }
                })
            }),
            std::time::Duration::from_secs(60),
        )
    }

    fn entry(id: &str, state: TaskState) -> TaskEntry {
        TaskEntry {
            task_id: uuid::Uuid::parse_str(id).unwrap(),
            state,
            created_at: chrono::Utc::now(),
            started_at: None,
            completed_at: None,
            result: None,
            error: None,
            request: None,
            source_meta: SourceMeta {
                mr_title: Some("MR".to_string()),
                ..SourceMeta::default()
            },
            progress: None,
            expert_name: None,
            llm_summary: None,
        }
    }

    /// Parse a local wall-clock time the same way regardless of the test
    /// process's timezone.
    fn local(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Local> {
        Local
            .with_ymd_and_hms(y, mo, d, h, mi, s)
            .single()
            .unwrap_or_else(|| panic!("invalid local fixture time {y}-{mo}-{d} {h}:{mi}:{s}"))
    }

    /// AppState wired like production startup (`state.db` set; task store
    /// write-throughs into the same in-memory SQLite DB).
    async fn state_with_db() -> (Arc<AppState>, Arc<SqlxStore>) {
        let db = Arc::new(SqlxStore::new_in_memory().await.unwrap());
        db.migrate().await.unwrap();
        let mut store = TaskStore::new();
        store.set_db(db.clone());
        let mut state = AppState::new(vec![]);
        state.task_store = Some(Arc::new(store));
        state.db = Some(db.clone());
        (Arc::new(state), db)
    }

    /// Seed one review row with a controlled creation time. `completed_in`
    /// sets `completed_at = created_at + completed_in` for Completed rows so
    /// `duration_ms()` is meaningful.
    async fn seed_review(
        db: &SqlxStore,
        state: TaskState,
        created_at: DateTime<Utc>,
        title: &str,
        completed_in: chrono::Duration,
    ) {
        let entry = TaskEntry {
            task_id: uuid::Uuid::new_v4(),
            state: state.clone(),
            created_at,
            started_at: Some(created_at),
            completed_at: (state == TaskState::Completed).then(|| created_at + completed_in),
            result: None,
            error: None,
            request: None,
            source_meta: SourceMeta {
                mr_title: Some(title.to_string()),
                project: Some("grp/proj".to_string()),
                author_name: Some("alice".to_string()),
                author_avatar_url: None,
                ..SourceMeta::default()
            },
            progress: None,
            expert_name: None,
            llm_summary: None,
        };
        db.create(&entry).await.unwrap();
    }

    async fn dashboard_json(state: Arc<AppState>) -> (StatusCode, serde_json::Value) {
        let resp = get_dashboard(State(state)).await.into_response();
        let status = resp.status();
        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
        (status, serde_json::from_slice(&body).unwrap())
    }

    // ─── window math (local-time anchors) ───

    /// ISO week anchor: a Wednesday lands on the Monday of the same week,
    /// expressed as Monday 00:00 LOCAL time.
    #[test]
    fn iso_week_start_is_monday_midnight_local() {
        // 2026-09-09 is a Wednesday; 2026-09-07 the Monday of that ISO week.
        let start = iso_week_start(local(2026, 9, 9, 15, 30, 0)).with_timezone(&Local);
        assert_eq!(start.date_naive(), NaiveDate::from_ymd_opt(2026, 9, 7).unwrap());
        assert_eq!(start.time(), chrono::NaiveTime::from_hms_opt(0, 0, 0).unwrap());
        assert_eq!(start.weekday(), chrono::Weekday::Mon);

        // A Monday morning is its own week start.
        let start = iso_week_start(local(2026, 9, 7, 0, 30, 0)).with_timezone(&Local);
        assert_eq!(start.date_naive(), NaiveDate::from_ymd_opt(2026, 9, 7).unwrap());

        // Sunday belongs to the ISO week that STARTED the previous Monday.
        let start = iso_week_start(local(2026, 9, 13, 12, 0, 0)).with_timezone(&Local);
        assert_eq!(start.date_naive(), NaiveDate::from_ymd_opt(2026, 9, 7).unwrap());
    }

    /// Day anchor: 23:59 local belongs to the day that began at 00:00 local.
    #[test]
    fn day_start_is_local_midnight() {
        let start = day_start(local(2026, 9, 9, 23, 59, 59)).with_timezone(&Local);
        assert_eq!(start.date_naive(), NaiveDate::from_ymd_opt(2026, 9, 9).unwrap());
        assert_eq!(start.time(), chrono::NaiveTime::from_hms_opt(0, 0, 0).unwrap());
    }

    // ─── delta math ───

    #[test]
    fn pct_change_is_relative_percent_with_null_for_empty_comparison() {
        assert_eq!(pct_change(5, 4), Some(25.0));
        assert_eq!(pct_change(4, 5), Some(-20.0));
        assert_eq!(pct_change(3, 3), Some(0.0));
        // Empty comparison window → no number, never a fake 0.0.
        assert_eq!(pct_change(3, 0), None);
        assert_eq!(pct_change(0, 0), None);
    }

    #[test]
    fn rate_delta_is_percentage_points_and_null_without_both_windows() {
        assert_eq!(rate_delta(Some(100.0), Some(80.0)), Some(20.0));
        assert_eq!(rate_delta(Some(50.0), Some(80.0)), Some(-30.0));
        assert_eq!(rate_delta(None, Some(80.0)), None);
        assert_eq!(rate_delta(Some(100.0), None), None);
        assert_eq!(rate_delta(None, None), None);
    }

    #[test]
    fn success_rate_is_null_without_terminal_reviews() {
        assert_eq!(success_rate(9, 1), Some(90.0));
        assert_eq!(success_rate(0, 4), Some(0.0));
        assert_eq!(success_rate(0, 0), None);
    }

    // ─── DB-backed aggregation ───

    /// reviewsThisWeek counts DB rows in the current ISO week; the WoW
    /// trend compares against the previous ISO week; rows outside both are
    /// ignored.
    #[tokio::test]
    async fn dashboard_db_week_counts_and_wow_trend() {
        let (state, db) = state_with_db().await;
        let week_start = iso_week_start(Local::now());
        // Last week: 1 review (2 days before the week start, still last week).
        seed_review(
            &db,
            TaskState::Completed,
            week_start - chrono::Duration::days(2),
            "last-week",
            chrono::Duration::seconds(30),
        )
        .await;
        // This week: 3 reviews, one of them failed.
        for i in 0..3 {
            let state = if i == 0 {
                TaskState::Failed
            } else {
                TaskState::Completed
            };
            seed_review(
                &db,
                state,
                week_start + chrono::Duration::hours(i + 1),
                "this-week",
                chrono::Duration::seconds(30),
            )
            .await;
        }
        // Ancient history: must not count anywhere.
        seed_review(
            &db,
            TaskState::Completed,
            week_start - chrono::Duration::days(40),
            "old",
            chrono::Duration::seconds(30),
        )
        .await;

        let (status, json) = dashboard_json(state).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["kpis"]["reviewsThisWeek"], 3);
        // WoW: (3 - 1) / 1 * 100 = +200%.
        assert_eq!(json["kpis"]["reviewsTrend"], 200.0);
        // successRate window is the current week: 2/(2+1) = 66.7%.
        assert_eq!(json["kpis"]["successRate"], 66.7);
        // avgDurationMs: 30s per completed review.
        assert_eq!(json["kpis"]["avgDurationMs"], 30_000);
        // Last week avg is also 30s → durationTrend 0.0 (both windows have data).
        assert_eq!(json["kpis"]["durationTrend"], 0.0);
    }

    /// Null-delta case: nothing in the comparison window → JSON null (the
    /// frontend renders "—"), NOT 0.0.
    #[tokio::test]
    async fn dashboard_null_deltas_when_comparison_window_empty() {
        let (state, db) = state_with_db().await;
        let today_start = day_start(Local::now());
        // Only this week, only today: yesterday / last week are empty.
        seed_review(
            &db,
            TaskState::Completed,
            today_start + chrono::Duration::hours(1),
            "today",
            chrono::Duration::seconds(30),
        )
        .await;

        let (status, json) = dashboard_json(state).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["kpis"]["reviewsThisWeek"], 1);
        assert!(
            json["kpis"]["reviewsTrend"].is_null(),
            "no last-week data → null, not 0.0"
        );
        assert!(json["kpis"]["successTrend"].is_null(), "no yesterday data → null");
        assert!(json["kpis"]["durationTrend"].is_null(), "no last-week avg → null");
    }

    /// RENG-32 sanity simulation: a review created 2 days ago and one 3
    /// hours ago produce a 24h trend with exactly one non-zero bucket, and
    /// the this-week count matches the ISO-week window.
    #[tokio::test]
    async fn dashboard_24h_trend_buckets_from_db_rows() {
        let (state, db) = state_with_db().await;
        let now = Utc::now();
        let three_hours_ago = now - chrono::Duration::hours(3);
        let two_days_ago = now - chrono::Duration::days(2);
        seed_review(
            &db,
            TaskState::Completed,
            three_hours_ago,
            "recent",
            chrono::Duration::seconds(30),
        )
        .await;
        seed_review(
            &db,
            TaskState::Completed,
            two_days_ago,
            "older",
            chrono::Duration::seconds(30),
        )
        .await;

        let (status, json) = dashboard_json(state).await;
        assert_eq!(status, StatusCode::OK);
        let trend = json["trend"].as_array().unwrap();
        assert_eq!(trend.len(), 24, "24 hourly buckets");
        let non_zero: Vec<&serde_json::Value> = trend.iter().filter(|p| p["value"].as_u64().unwrap_or(0) > 0).collect();
        assert_eq!(non_zero.len(), 1, "exactly one bucket holds the 3h-old review");
        assert_eq!(non_zero[0]["value"], 1);
        let bucket_time = non_zero[0]["time"].as_i64().unwrap();
        assert!(
            bucket_time <= three_hours_ago.timestamp() + 3600 && bucket_time > three_hours_ago.timestamp() - 3600,
            "the non-zero bucket must be the one covering ~3h ago (bucket end {bucket_time}, review at {})",
            three_hours_ago.timestamp()
        );
        // This-week count follows the same ISO-week window as the week KPI.
        let expected_this_week = (two_days_ago >= iso_week_start(Local::now())) as u64
            + (three_hours_ago >= iso_week_start(Local::now())) as u64;
        assert_eq!(json["kpis"]["reviewsThisWeek"].as_u64().unwrap(), expected_this_week);
    }

    /// RENG-50: the daily series buckets by LOCAL CALENDAR DAY — a review at
    /// Aug 31 02:00 local and one at Sep 1 02:00 local land in two different
    /// buckets (the month boundary is a midnight line, not a multiple of
    /// 86400 s from "now"), each `time` is that day's local midnight, and the
    /// window is the 14 local days ending today. Fixed anchor keeps this
    /// deterministic regardless of the host timezone or when it runs.
    #[test]
    fn daily_trend_buckets_by_local_calendar_day() {
        let now = local(2026, 9, 13, 15, 30, 0);
        let aug31 = local_midnight(NaiveDate::from_ymd_opt(2026, 8, 31).unwrap(), now);
        let sep1 = local_midnight(NaiveDate::from_ymd_opt(2026, 9, 1).unwrap(), now);
        let mut aug31_review = entry("00000000-0000-0000-0000-000000000010", TaskState::Completed);
        aug31_review.created_at = aug31 + chrono::Duration::hours(2);
        let mut sep1_review = entry("00000000-0000-0000-0000-000000000011", TaskState::Completed);
        sep1_review.created_at = sep1 + chrono::Duration::hours(2);

        let points = compute_trend_daily(&[aug31_review, sep1_review], now);
        assert_eq!(points.len(), 14, "exactly 14 daily points");
        // Window: Aug 31 ..= Sep 13 (14 local days, today last), oldest first —
        // so the Aug 31 review lands in the FIRST bucket.
        assert_eq!(
            points[0]["time"].as_i64().unwrap(),
            local_midnight(NaiveDate::from_ymd_opt(2026, 8, 31).unwrap(), now).timestamp()
        );
        assert_eq!(
            points[13]["time"].as_i64().unwrap(),
            local_midnight(NaiveDate::from_ymd_opt(2026, 9, 13).unwrap(), now).timestamp()
        );
        // Month boundary: each review sits in its own day's bucket, keyed by
        // that day's local-midnight timestamp.
        let by_time: std::collections::HashMap<i64, u64> = points
            .iter()
            .map(|p| (p["time"].as_i64().unwrap(), p["value"].as_u64().unwrap()))
            .collect();
        assert_eq!(by_time[&aug31.timestamp()], 1, "Aug 31 review → Aug 31 bucket");
        assert_eq!(by_time[&sep1.timestamp()], 1, "Sep 1 review → Sep 1 bucket");
        assert_eq!(
            by_time.values().map(|v| *v).sum::<u64>(),
            2,
            "no review is double-counted across the boundary"
        );
        // Oldest → newest ordering, one midnight-step apart.
        for w in points.windows(2) {
            let (a, b) = (w[0]["time"].as_i64().unwrap(), w[1]["time"].as_i64().unwrap());
            assert!(a < b, "points must be strictly increasing");
        }
    }

    /// Empty DB: `trendDaily` is still 14 zero points on the correct local
    /// midnight anchors — first = 13 days ago's midnight, last = today's
    /// midnight, i.e. the same `day_start` anchor the KPI today/yesterday
    /// windows use.
    #[tokio::test]
    async fn dashboard_daily_trend_empty_db_14_zero_points() {
        let (state, _db) = state_with_db().await;
        let (status, json) = dashboard_json(state).await;
        assert_eq!(status, StatusCode::OK);
        let daily = json["trendDaily"].as_array().unwrap();
        assert_eq!(daily.len(), 14, "exactly 14 daily points");
        assert!(
            daily.iter().all(|p| p["value"] == 0),
            "empty DB → all-zero values, never fabricated counts"
        );
        let now = Local::now();
        assert_eq!(
            daily[13]["time"].as_i64().unwrap(),
            day_start(now).timestamp(),
            "today's local midnight is the LAST point (same anchor as the KPI today window)"
        );
        assert_eq!(
            daily[0]["time"].as_i64().unwrap(),
            local_midnight(now.date_naive() - chrono::Duration::days(13), now).timestamp(),
            "first point is 13 days ago's local midnight"
        );
        for w in daily.windows(2) {
            assert!(w[0]["time"].as_i64().unwrap() < w[1]["time"].as_i64().unwrap());
        }
        // The 24h series is untouched by the addition.
        assert_eq!(json["trend"].as_array().unwrap().len(), 24);
    }

    /// End-to-end from the DB: reviews seeded at 14 days ago (OUTSIDE the
    /// window), the first bucket's day, yesterday late night, and this
    /// morning produce exactly the expected buckets — today is the last
    /// point and holds this morning's review.
    #[tokio::test]
    async fn dashboard_daily_trend_counts_db_rows_with_today_last() {
        let (state, db) = state_with_db().await;
        let now = Local::now();
        let today_start = day_start(now);
        let first_day_start = local_midnight(now.date_naive() - chrono::Duration::days(13), now);
        // 14 days ago: one day BEFORE the window → must not appear anywhere.
        seed_review(
            &db,
            TaskState::Completed,
            today_start - chrono::Duration::days(14),
            "too-old",
            chrono::Duration::seconds(30),
        )
        .await;
        // First bucket (13 days ago), yesterday 23:30, today 09:00.
        for (ts, title) in [
            (first_day_start + chrono::Duration::hours(9), "first-day"),
            (today_start - chrono::Duration::minutes(30), "yesterday-late"),
            (today_start + chrono::Duration::hours(9), "this-morning"),
        ] {
            seed_review(&db, TaskState::Completed, ts, title, chrono::Duration::seconds(30)).await;
        }

        let (status, json) = dashboard_json(state).await;
        assert_eq!(status, StatusCode::OK);
        let daily = json["trendDaily"].as_array().unwrap();
        assert_eq!(daily.len(), 14);
        assert_eq!(daily[0]["value"], 1, "first bucket holds the 13-days-ago review");
        assert_eq!(daily[12]["value"], 1, "yesterday 23:30 → yesterday's bucket");
        assert_eq!(daily[13]["time"].as_i64().unwrap(), today_start.timestamp());
        assert_eq!(daily[13]["value"], 1, "today is the LAST point and is partial");
        assert_eq!(
            daily.iter().map(|p| p["value"].as_u64().unwrap()).sum::<u64>(),
            3,
            "the 14-days-ago review is outside the window"
        );
    }

    /// recentReviews comes from the DB, newest first, capped at 5, with the
    /// fields the cards render.
    #[tokio::test]
    async fn dashboard_recent_reviews_from_db_newest_first_capped() {
        let (state, db) = state_with_db().await;
        let now = Utc::now();
        for i in 0..7 {
            seed_review(
                &db,
                TaskState::Completed,
                now - chrono::Duration::hours(i + 1),
                &format!("review-{i}"),
                chrono::Duration::seconds(42),
            )
            .await;
        }

        let (status, json) = dashboard_json(state).await;
        assert_eq!(status, StatusCode::OK);
        let recent = json["recentReviews"].as_array().unwrap();
        assert_eq!(recent.len(), 5, "capped at RECENT_REVIEWS_LIMIT");
        assert_eq!(recent[0]["mrTitle"], "review-0", "newest first");
        assert_eq!(recent[0]["project"], "grp/proj");
        assert_eq!(recent[0]["author"]["name"], "alice");
        assert_eq!(recent[0]["status"], "completed");
        assert_eq!(recent[0]["durationMs"], 42_000);
        assert!(recent[0]["createdAt"].is_string());
        assert_eq!(recent[4]["mrTitle"], "review-4");
    }

    /// activeQueue stays LIVE: it counts pending/running entries from the
    /// in-memory task store even though history comes from the DB (where
    /// those same tasks only appear as persisted history rows).
    #[tokio::test]
    async fn dashboard_active_queue_from_task_store_not_db() {
        let (state, _db) = state_with_db().await;
        let store = state.task_store.clone().unwrap();
        let pending = store.create(None).await;
        store.update(pending, TaskState::Pending, None, None).await;
        let running = store.create(None).await;
        store.update(running, TaskState::Running, None, None).await;
        let done = store.create(None).await;
        store.update(done, TaskState::Completed, None, None).await;

        let (status, json) = dashboard_json(state).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["kpis"]["activeQueue"], 2, "pending + running only");
    }

    /// Today's success rate vs yesterday's: the trend is a percentage-point
    /// delta, null when yesterday has no terminal reviews.
    #[tokio::test]
    async fn dashboard_success_trend_is_pp_delta_vs_yesterday() {
        let (state, db) = state_with_db().await;
        let today_start = day_start(Local::now());
        let yesterday_start = today_start - chrono::Duration::days(1);
        // Today: 1/1 completed → 100%. Yesterday: 1 completed + 3 failed → 25%.
        seed_review(
            &db,
            TaskState::Completed,
            today_start + chrono::Duration::hours(1),
            "t1",
            chrono::Duration::seconds(30),
        )
        .await;
        seed_review(
            &db,
            TaskState::Completed,
            yesterday_start + chrono::Duration::hours(1),
            "y1",
            chrono::Duration::seconds(30),
        )
        .await;
        for i in 0..3 {
            seed_review(
                &db,
                TaskState::Failed,
                yesterday_start + chrono::Duration::hours(i + 2),
                "yf",
                chrono::Duration::seconds(30),
            )
            .await;
        }

        let (status, json) = dashboard_json(state).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            json["kpis"]["successTrend"], 75.0,
            "100% today vs 25% yesterday = +75 pp"
        );
    }

    /// Health panel (RENG-97): integration detection reads the platform probe
    /// cache, NOT entry presence — a configured-but-never-probed instance
    /// reads `unknown` (honest), never `success`. No fabricated `latencyMs`
    /// appears on an unprobed row (the only latency is a real probe's).
    #[tokio::test]
    async fn dashboard_health_detects_configured_git_platforms() {
        let (state, _db) = state_with_db().await;
        state
            .git_platforms
            .write()
            .unwrap()
            .push(crate::models::GitPlatformConfig {
                name: "testbed".to_string(),
                platform_type: "gitlab".to_string(),
                ..Default::default()
            });

        let (status, json) = dashboard_json(state).await;
        assert_eq!(status, StatusCode::OK);
        let integrations = json["health"]["integrations"].as_array().unwrap();
        let by_service: std::collections::HashMap<&str, &serde_json::Value> = integrations
            .iter()
            .map(|i| (i["service"].as_str().unwrap(), i))
            .collect();
        assert_eq!(
            by_service["GitLab API"]["status"], "unknown",
            "an entry that was never probed must not read success: {json}"
        );
        assert_eq!(by_service["GitLab API"]["message"], "Not probed yet");
        assert_eq!(by_service["GitHub API"]["status"], "offline");
        for item in integrations {
            assert!(
                item.get("latencyMs").is_none(),
                "an unprobed row has no latency: {item}"
            );
        }
        // LLM rows are absent here (no provider configured) and carry no latencyMs.
        let llm = json["health"]["llmProviders"].as_array().unwrap();
        assert!(llm.is_empty(), "no LLM configured in this state");
        assert_eq!(json["health"]["overall"], "offline");
    }

    /// The env/CLI channel alone (`GITLAB_TOKEN` / `--gitlab-token` recorded
    /// at startup) configures an integration with no `git_platforms` entry —
    /// and therefore nothing the Configuration probe can test. Presence must
    /// not read as health: the row is `unknown` (never probed), not `success`.
    #[tokio::test]
    async fn dashboard_health_env_flag_alone_reads_unknown() {
        let (mut state, _db) = state_with_db().await;
        // No git_platforms entries; only the startup-recorded env flag.
        Arc::get_mut(&mut state).unwrap().env_gitlab_configured = true;

        let (status, json) = dashboard_json(state).await;
        assert_eq!(status, StatusCode::OK);
        let integrations = json["health"]["integrations"].as_array().unwrap();
        let by_service: std::collections::HashMap<&str, &serde_json::Value> = integrations
            .iter()
            .map(|i| (i["service"].as_str().unwrap(), i))
            .collect();
        assert_eq!(
            by_service["GitLab API"]["status"], "unknown",
            "env-configured-but-never-probed must not read success: {json}"
        );
        assert_eq!(by_service["GitLab API"]["message"], "Not probed yet");
        assert_eq!(by_service["GitHub API"]["status"], "offline");
    }

    /// RENG-97: an entry whose probe failed reads `error` on the dashboard —
    /// the same 401 the Configuration page showed — never `success` from the
    /// entry still existing.
    #[tokio::test]
    async fn dashboard_health_failed_probe_reports_error_not_success() {
        let (state, _db) = state_with_db().await;
        let gitlab = crate::models::GitPlatformConfig {
            name: "testbed".to_string(),
            platform_type: "gitlab".to_string(),
            base_url: "https://gitlab.example".to_string(),
            token: "glpat-broken".to_string(),
            ..Default::default()
        };
        *state.git_platforms.write().unwrap() = vec![gitlab.clone()];
        state.git_health.record(
            &gitlab.base_url,
            &gitlab.token,
            crate::server::api::git_health::GitPlatformHealth::error("HTTP 401 Unauthorized"),
        );

        let (status, json) = dashboard_json(state).await;
        assert_eq!(status, StatusCode::OK);
        let integrations = json["health"]["integrations"].as_array().unwrap();
        let by_service: std::collections::HashMap<&str, &serde_json::Value> = integrations
            .iter()
            .map(|i| (i["service"].as_str().unwrap(), i))
            .collect();
        assert_eq!(
            by_service["GitLab API"]["status"], "error",
            "a failed probe must not read success: {json}"
        );
        assert_eq!(by_service["GitLab API"]["message"], "HTTP 401 Unauthorized");
        assert!(
            by_service["GitLab API"]["checkedAt"].as_str().is_some(),
            "a failure carries its probe time: {json}"
        );
        assert!(by_service["GitLab API"].get("latencyMs").is_none());
    }

    /// RENG-97: after a successful probe the dashboard row reports the real
    /// latency and timestamp — the same verdict the Configuration page showed.
    #[tokio::test]
    async fn dashboard_health_successful_probe_reports_success_with_latency() {
        let (state, _db) = state_with_db().await;
        let gitlab = crate::models::GitPlatformConfig {
            name: "testbed".to_string(),
            platform_type: "gitlab".to_string(),
            base_url: "https://gitlab.example".to_string(),
            token: "glpat-good".to_string(),
            ..Default::default()
        };
        *state.git_platforms.write().unwrap() = vec![gitlab.clone()];
        state.git_health.record(
            &gitlab.base_url,
            &gitlab.token,
            crate::server::api::git_health::GitPlatformHealth::healthy(Some("16.9.0".to_string()), 37),
        );

        let (status, json) = dashboard_json(state).await;
        assert_eq!(status, StatusCode::OK);
        let integrations = json["health"]["integrations"].as_array().unwrap();
        let by_service: std::collections::HashMap<&str, &serde_json::Value> = integrations
            .iter()
            .map(|i| (i["service"].as_str().unwrap(), i))
            .collect();
        assert_eq!(by_service["GitLab API"]["status"], "success");
        assert_eq!(by_service["GitLab API"]["latencyMs"], 37);
        assert!(by_service["GitLab API"]["checkedAt"].as_str().is_some());
        assert_eq!(
            by_service["GitHub API"]["status"], "offline",
            "the unconfigured platform stays offline: {json}"
        );
    }

    /// Neither channel configured (no platform entries, no env/CLI tokens)
    /// → both integrations offline.
    #[tokio::test]
    async fn dashboard_health_offline_without_platform_or_env_flag() {
        let (state, _db) = state_with_db().await;

        let (status, json) = dashboard_json(state).await;
        assert_eq!(status, StatusCode::OK);
        let integrations = json["health"]["integrations"].as_array().unwrap();
        let by_service: std::collections::HashMap<&str, &serde_json::Value> = integrations
            .iter()
            .map(|i| (i["service"].as_str().unwrap(), i))
            .collect();
        assert_eq!(by_service["GitLab API"]["status"], "offline");
        assert_eq!(by_service["GitHub API"]["status"], "offline");
    }

    /// The LLM provider health rows survive without latencyMs.
    #[tokio::test]
    async fn dashboard_health_llm_rows_have_no_latency() {
        let mut state = AppState::new(vec![crate::models::LLMConfig {
            provider: "openai".to_string(),
            model: "gpt-4".to_string(),
            api_key: "sk-test".to_string(),
            api_base: "https://api.openai.com".to_string(),
            max_tokens: 4096,
            temperature: 0.7,
            disable_thinking: None,
            disabled: false,
        }]);
        // RENG-36: the row's status is the probe's verdict, so pin it with a
        // stub probe (a real one would hit api.openai.com from a unit test).
        state.llm_health = Arc::new(stub_health_store(true));
        let (status, json) = dashboard_json(Arc::new(state)).await;
        assert_eq!(status, StatusCode::OK);
        let llm = &json["health"]["llmProviders"][0];
        assert_eq!(llm["service"], "openai gpt-4");
        assert_eq!(llm["status"], "success");
        assert_eq!(llm["message"], "Configured");
        assert!(llm.get("latencyMs").is_none());
        assert_eq!(json["health"]["overall"], "success");
    }

    /// RENG-36: the dashboard's LLM rows and `overall` follow the probe
    /// verdicts, not key presence — a provider whose probe fails (the reported
    /// symptom: a key broken in the Web UI while the service keeps running)
    /// must not read as operational, and a mixed set is `warning`.
    #[tokio::test]
    async fn dashboard_health_follows_probe_verdicts() {
        let llm = |provider: &str, key: &str| crate::models::LLMConfig {
            provider: provider.to_string(),
            model: format!("{provider}-model"),
            api_key: key.to_string(),
            api_base: format!("https://api.{provider}.example/v1"),
            max_tokens: 4096,
            temperature: 0.7,
            disable_thinking: None,
            disabled: false,
        };

        // One healthy provider + one failing + one without a key.
        let mut state = AppState::new(vec![llm("openai", "sk-a"), llm("deepseek", "sk-b"), llm("ollama", "")]);
        state.llm_health = Arc::new(per_provider_health_store());
        let (status, json) = dashboard_json(Arc::new(state)).await;
        assert_eq!(status, StatusCode::OK);
        let rows = json["health"]["llmProviders"].as_array().unwrap();
        assert_eq!(rows[0]["status"], "success");
        assert_eq!(rows[1]["status"], "error");
        assert_eq!(rows[1]["message"], "HTTP 401 Unauthorized");
        assert_eq!(rows[2]["status"], "offline");
        assert_eq!(rows[2]["message"], "Missing API key");
        assert_eq!(
            json["health"]["overall"], "warning",
            "one failed provider among healthy ones is degraded, not operational"
        );

        // Every provider failing → `error` (never `success`).
        let mut state = AppState::new(vec![llm("openai", "sk-a")]);
        state.llm_health = Arc::new(stub_health_store(false));
        let (_, json) = dashboard_json(Arc::new(state)).await;
        assert_eq!(json["health"]["llmProviders"][0]["status"], "error");
        assert_eq!(json["health"]["overall"], "error");
    }

    /// RENG-75: a disabled provider's dashboard row reads `disabled` —
    /// deliberately off, not a failure — and it casts no vote on `overall`:
    /// a healthy enabled provider keeps the panel `success`, and with every
    /// provider disabled the subsystem is simply not serving (`offline`,
    /// never `error`).
    #[tokio::test]
    async fn dashboard_health_marks_disabled_providers_without_failing_overall() {
        let llm = |provider: &str, disabled: bool| crate::models::LLMConfig {
            provider: provider.to_string(),
            model: format!("{provider}-model"),
            api_key: "sk-a".to_string(),
            api_base: format!("https://api.{provider}.example/v1"),
            max_tokens: 4096,
            temperature: 0.7,
            disable_thinking: None,
            disabled,
        };

        let mut state = AppState::new(vec![llm("openai", false), llm("deepseek", true)]);
        state.llm_health = Arc::new(stub_health_store(true));
        let (status, json) = dashboard_json(Arc::new(state)).await;
        assert_eq!(status, StatusCode::OK);
        let rows = json["health"]["llmProviders"].as_array().unwrap();
        assert_eq!(rows[0]["status"], "success");
        assert_eq!(rows[1]["status"], "disabled", "deliberately off ≠ offline/error");
        assert_eq!(rows[1]["message"], "Disabled");
        assert_eq!(
            json["health"]["overall"], "success",
            "the disabled provider must not drag the panel to warning/error"
        );

        // All disabled → nothing serving, but nothing failing either.
        let mut state = AppState::new(vec![llm("openai", true)]);
        state.llm_health = Arc::new(stub_health_store(true));
        let (_, json) = dashboard_json(Arc::new(state)).await;
        assert_eq!(json["health"]["llmProviders"][0]["status"], "disabled");
        assert_eq!(json["health"]["overall"], "offline");
    }

    /// `None` fallback: without ANY store the dashboard serves documented
    /// defaults (zero counts, null trends/derived values, empty recent
    /// reviews) instead of 503 — a pure guard, unreachable in production.
    #[tokio::test]
    async fn dashboard_without_store_serves_defaults() {
        let mut state = AppState::new(vec![]);
        state.task_store = None;
        let (status, json) = dashboard_json(Arc::new(state)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["kpis"]["reviewsThisWeek"], 0);
        assert_eq!(json["kpis"]["activeQueue"], 0);
        assert!(json["kpis"]["reviewsTrend"].is_null());
        assert!(json["kpis"]["successRate"].is_null());
        assert!(json["recentReviews"].as_array().unwrap().is_empty());
        let daily = json["trendDaily"].as_array().unwrap();
        assert_eq!(daily.len(), 14, "default daily series keeps the 14-point shape");
        assert!(daily.iter().all(|p| p["value"] == 0));
        assert_eq!(json["health"]["overall"], "offline");
    }

    /// `recentReviews` reports the real task state vocabulary
    /// (pending/running/completed/failed/cancelled), consistent with `/reviews`,
    /// and surfaces every state instead of only completed/failed.
    #[test]
    fn recent_reviews_use_real_status_vocabulary() {
        let items = vec![
            entry("00000000-0000-0000-0000-000000000001", TaskState::Pending),
            entry("00000000-0000-0000-0000-000000000002", TaskState::Running),
            entry("00000000-0000-0000-0000-000000000003", TaskState::Completed),
            entry("00000000-0000-0000-0000-000000000004", TaskState::Failed),
            entry("00000000-0000-0000-0000-000000000005", TaskState::Cancelled),
        ];
        let recent = compute_recent_reviews(&items);
        assert_eq!(recent.len(), 5, "every state must surface in recentReviews");

        let by_id: std::collections::HashMap<String, String> = recent
            .iter()
            .map(|r| {
                (
                    r["id"].as_str().unwrap().to_string(),
                    r["status"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        assert_eq!(by_id["00000000-0000-0000-0000-000000000001"], "pending");
        assert_eq!(by_id["00000000-0000-0000-0000-000000000002"], "running");
        assert_eq!(by_id["00000000-0000-0000-0000-000000000003"], "completed");
        assert_eq!(by_id["00000000-0000-0000-0000-000000000004"], "failed");
        assert_eq!(by_id["00000000-0000-0000-0000-000000000005"], "cancelled");
    }

    /// With only the in-memory store (db=None, the REVIEW_DISABLE_DB=1
    /// configuration) the dashboard aggregates from it: a completed review
    /// from the webhook path surfaces as reviewsThisWeek + recentReviews,
    /// with null trends because the comparison windows are empty.
    #[tokio::test]
    async fn dashboard_with_store_shows_webhook_recorded_review() {
        let state = AppState::new(vec![]);
        let store = state.task_store.clone().unwrap();
        let id = store.create(None).await;
        store.update(id, TaskState::Completed, None, None).await;

        let (status, json) = dashboard_json(Arc::new(state)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["kpis"]["reviewsThisWeek"], 1);
        let recent = json["recentReviews"].as_array().unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0]["id"], id.to_string());
        assert_eq!(recent[0]["status"], "completed");
        assert!(json["kpis"]["reviewsTrend"].is_null());
    }
}
