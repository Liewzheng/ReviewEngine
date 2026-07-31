import { request } from './api';
import { normalizeStatus } from './status';
import type { KpiData, TrendPoint, SystemHealth, RecentReview } from '../types/dashboard';

export interface DashboardResponse {
  kpis: KpiData;
  trend: TrendPoint[];
  health: SystemHealth;
  recentReviews: RecentReview[];
}

export async function getDashboard(): Promise<DashboardResponse> {
  const data = await request<DashboardResponse>('/dashboard');
  // The backend now reports each task's real status (`pending`/`running`/...).
  // Normalize `pending` -> `queued` here so components never see the raw string.
  return {
    ...data,
    recentReviews: data.recentReviews.map((r) => ({ ...r, status: normalizeStatus(r.status) })),
  };
}
