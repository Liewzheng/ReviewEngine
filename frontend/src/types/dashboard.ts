export interface KpiData {
  reviewsThisWeek: number;
  reviewsTrend: number;
  activeQueue: number;
  successRate: number;
  successTrend: number;
  avgDurationMs: number;
  durationTrend: number;
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
  latencyMs?: number;
  message?: string;
}

export interface SystemHealth {
  integrations: HealthStatus[];
  llmProviders: HealthStatus[];
  overall: HealthState;
  lastChecked: string;
  /** False when the server has no usable LLM configured (reviews cannot run). */
  llmConfigured: boolean;
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
