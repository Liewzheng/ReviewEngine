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
  time: string;
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
}

export type ReviewStatus = 'success' | 'failed' | 'running' | 'queued';

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
