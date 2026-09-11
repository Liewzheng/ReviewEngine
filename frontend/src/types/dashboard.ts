export interface KpiData {
  reviewsThisWeek: number;
  /** WoW relative change (%) vs last week; null when last week had no reviews — rendered as "—". */
  reviewsTrend: number | null;
  activeQueue: number;
  /** completed/(completed+failed) this week (%); null when the week has no terminal reviews. */
  successRate: number | null;
  /** Percentage-point delta vs yesterday; null when either day has no terminal reviews. */
  successTrend: number | null;
  /** Average duration of completed reviews this week; null when there were none. */
  avgDurationMs: number | null;
  /** WoW relative change (%) of the weekly average duration; null without both weeks' data. */
  durationTrend: number | null;
}

export interface TrendPoint {
  time: number; // Unix timestamp in seconds
  value: number;
}

export type HealthStatusType = 'integration' | 'llm';
export type HealthState = 'success' | 'warning' | 'error' | 'offline';

export interface HealthStatus {
  service: string;
  type: HealthStatusType;
  status: HealthState;
  /** Free-form probe detail (e.g. "Configured" / "Missing API key"). */
  message?: string;
}

/**
 * Persistence backend in use, reported by `/system/health` (0.10.0).
 * Absent when the server predates the field.
 */
export type StorageBackendKind = 'postgresql' | 'sqlite' | 'disabled';

export interface SystemHealth {
  integrations: HealthStatus[];
  llmProviders: HealthStatus[];
  overall: HealthState;
  lastChecked: string;
  /** False when the server has no usable LLM configured (reviews cannot run). */
  llmConfigured: boolean;
  /** Persistence backend kind; normalized from the raw `storage_backend` key. */
  storageBackend?: StorageBackendKind;
}

// Display-facing status for recent reviews. The backend reports the real task
// vocabulary (`pending`/`running`/`completed`/`failed`/`cancelled`); the
// service layer (`services/dashboard.ts`) normalizes `pending` -> `queued`.
// `success` is retained as a legacy value from older dashboard responses.
export type ReviewStatus = 'success' | 'failed' | 'running' | 'queued' | 'completed' | 'cancelled';

export interface ReviewAuthor {
  name: string;
  avatarUrl?: string;
}

export interface RecentReview {
  id: string;
  mrTitle: string;
  project: string;
  author: ReviewAuthor;
  status: ReviewStatus;
  durationMs: number;
  createdAt: string;
}

export interface DashboardState {
  kpis: KpiData | null;
  trend: TrendPoint[];
  health: SystemHealth | null;
  recentReviews: RecentReview[];
  loading: boolean;
  lastUpdated: string | null;
}
