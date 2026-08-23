import { request } from './api';
import { normalizeStatus } from './status';
import type { ReviewListItem, ReviewDetail, HistoryFilters, RiskLevel, ReviewAssessment } from '../types/history';

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
  /** Embedded full `ReviewOutput` JSON (carries `consolidated.assessment`). */
  result?: unknown;
}

/** Fallback band derivation, mirroring `scoring::review::score_to_risk_level`. */
function riskLevelFromScore(score: number): RiskLevel {
  if (score >= 91) return 'healthy';
  if (score >= 81) return 'low';
  if (score >= 61) return 'low-medium';
  if (score >= 41) return 'medium';
  if (score >= 21) return 'high';
  return 'critical';
}

/**
 * The backend serializes `risk_level` either as serde variant names
 * (`"LowMedium"`, MR reviews) or lowercase display strings (`"low-medium"`,
 * repo-side adapter) depending on the code path — accept both.
 */
function normalizeRiskLevel(raw: unknown, score: number): RiskLevel {
  const s = typeof raw === 'string' ? raw.toLowerCase() : '';
  switch (s) {
    case 'healthy':
      return 'healthy';
    case 'low':
      return 'low';
    case 'lowmedium':
    case 'low-medium':
      return 'low-medium';
    case 'medium':
      return 'medium';
    case 'high':
      return 'high';
    case 'critical':
      return 'critical';
    default:
      return riskLevelFromScore(score);
  }
}

/**
 * Extract the lead-consolidation overall assessment from an embedded
 * `ReviewOutput` (`consolidated.assessment`). Absent for non-completed tasks
 * and results without consolidation — the UI hides the score then.
 */
export function extractAssessment(result: unknown): ReviewAssessment | undefined {
  if (!result || typeof result !== 'object') return undefined;
  const assessment = (result as {
    consolidated?: { assessment?: { score?: unknown; risk_level?: unknown; unverified?: unknown } } | null;
  }).consolidated?.assessment;
  if (!assessment || typeof assessment.score !== 'number') return undefined;
  return {
    score: assessment.score,
    riskLevel: normalizeRiskLevel(assessment.risk_level, assessment.score),
    unverified: assessment.unverified === true,
  };
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
    assessment: extractAssessment(raw.result),
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
  const raw = await request<ReviewDetail & { result?: unknown }>(`/reviews/${id}`);
  // The merged detail response carries the raw snake_case status string
  // (`pending`/`running`/...); normalize it to the display vocabulary.
  // `rawApiResponse` is the embedded `ReviewOutput` (same payload as the
  // snake_case `result` key); the assessment lives under
  // `consolidated.assessment` in it.
  return {
    ...raw,
    status: normalizeStatus(raw.status),
    assessment: extractAssessment(raw.rawApiResponse ?? raw.result),
  };
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
