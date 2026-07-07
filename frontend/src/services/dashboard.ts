import { request } from './api';
import type { KpiData, TrendPoint, SystemHealth, RecentReview } from '../types/dashboard';

export interface DashboardResponse {
  kpis: KpiData;
  trend: TrendPoint[];
  health: SystemHealth;
  recentReviews: RecentReview[];
}

export async function getDashboard(): Promise<DashboardResponse> {
  return request('/dashboard');
}
