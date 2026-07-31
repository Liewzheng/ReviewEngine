import { request } from './api';
import { normalizeStatus } from './status';
import type { ReviewListItem, ReviewDetail, HistoryFilters } from '../types/history';

export interface ReviewsListResponse {
  items: ReviewListItem[];
  total: number;
  page: number;
  per_page: number;
}

export interface RerunReviewResponse {
  taskId: string;
}

/**
 * Raw item shape as served by `GET /reviews`. The backend list endpoint
 * currently returns the snake_case `TaskStatus` shape (`task_id`, `mr_title`,
 * `author_name`, ...) — unlike `GET /reviews/{task_id}`, which merges camelCase
 * structured fields. This normalizer reads both shapes defensively so the list
 * page receives the camelCase `ReviewListItem` it renders.
 */
interface RawReviewItem {
  task_id?: string;
  id?: string;
  mr_title?: string | null;
  mrTitle?: string;
  project?: string | null;
  repository?: string | null;
  branch?: string | null;
  target_branch?: string | null;
  targetBranch?: string;
  author_name?: string | null;
  author_avatar_url?: string | null;
  author?: { name?: string; avatarUrl?: string };
  status?: string;
  duration_ms?: number | null;
  durationMs?: number;
  created_at?: string;
  createdAt?: string;
  gitlab_mr_url?: string | null;
  gitlabMrUrl?: string;
}

function normalizeReviewListItem(raw: RawReviewItem): ReviewListItem {
  return {
    id: raw.id ?? raw.task_id ?? '',
    mrTitle: raw.mrTitle ?? raw.mr_title ?? 'Untitled Review',
    project: raw.project ?? '',
    repository: raw.repository ?? '',
    branch: raw.branch ?? '',
    targetBranch: raw.targetBranch ?? raw.target_branch ?? '',
    author: {
      name: raw.author?.name ?? raw.author_name ?? 'unknown',
      avatarUrl: raw.author?.avatarUrl ?? raw.author_avatar_url ?? undefined,
    },
    status: normalizeStatus(raw.status),
    durationMs: raw.durationMs ?? raw.duration_ms ?? 0,
    createdAt: raw.createdAt ?? raw.created_at ?? '',
    gitlabMrUrl: raw.gitlabMrUrl ?? raw.gitlab_mr_url ?? undefined,
  };
}

export async function getReviews(
  filters: HistoryFilters,
  page: number = 1,
  perPage: number = 20
): Promise<ReviewsListResponse> {
  const params = new URLSearchParams();
  // The reviews API filters by `pending`, not `queued`; translate the
  // display-facing value before sending.
  const status = filters.status === 'queued' ? 'pending' : filters.status;
  if (status) params.append('status', status);
  if (filters.q) params.append('q', filters.q);
  if (filters.project) params.append('project', filters.project);
  if (filters.dateFrom) params.append('date_from', filters.dateFrom);
  if (filters.dateTo) params.append('date_to', filters.dateTo);
  if (filters.repository) params.append('repository', filters.repository);
  params.append('page', String(page));
  params.append('per_page', String(perPage));
  const data = await request<{ items: RawReviewItem[]; total: number; page: number; per_page: number }>(
    `/reviews?${params.toString()}`
  );
  return {
    ...data,
    items: data.items.map(normalizeReviewListItem),
  };
}

export async function getReview(id: string): Promise<ReviewDetail> {
  const raw = await request<ReviewDetail>(`/reviews/${id}`);
  // The merged detail response carries the raw snake_case status string
  // (`pending`/`running`/...); normalize it to the display vocabulary.
  return { ...raw, status: normalizeStatus(raw.status) };
}

export async function deleteReview(id: string): Promise<void> {
  await request(`/reviews/${id}`, { method: 'DELETE' });
}

/**
 * Re-run a settled review. The backend returns `{"task_id": "<uuid>"}` (202);
 * errors: 404 not found / 409 still running or original request unavailable /
 * 422 stored parameters not replayable.
 */
export async function rerunReview(id: string): Promise<RerunReviewResponse> {
  const data = await request<{ task_id: string }>(`/reviews/${id}/rerun`, { method: 'POST' });
  return { taskId: data.task_id };
}
