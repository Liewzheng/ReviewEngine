import { request } from './api';
import type { ReviewListItem, ReviewDetail, HistoryFilters } from '../types/history';

export interface ReviewsListResponse {
  items: ReviewListItem[];
  total: number;
  page: number;
  per_page: number;
}

export async function getReviews(
  filters: HistoryFilters,
  page: number = 1,
  perPage: number = 20
): Promise<ReviewsListResponse> {
  const params = new URLSearchParams();
  if (filters.status) params.append('status', filters.status);
  if (filters.q) params.append('q', filters.q);
  if (filters.project) params.append('project', filters.project);
  if (filters.dateFrom) params.append('date_from', filters.dateFrom);
  if (filters.dateTo) params.append('date_to', filters.dateTo);
  if (filters.repository) params.append('repository', filters.repository);
  params.append('page', String(page));
  params.append('per_page', String(perPage));
  return request(`/reviews?${params.toString()}`);
}

export async function getReview(id: string): Promise<ReviewDetail> {
  return request(`/reviews/${id}`);
}

export async function deleteReview(id: string): Promise<void> {
  await request(`/reviews/${id}`, { method: 'DELETE' });
}

export async function rerunReview(id: string): Promise<{ taskId: string; status: string }> {
  return request(`/reviews/${id}/rerun`, { method: 'POST' });
}
