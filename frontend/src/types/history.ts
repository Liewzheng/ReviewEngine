// Display-facing review status. The API emits `pending` for queued tasks; the
// service layer (`services/reviews.ts`) maps `pending` -> `queued` so the UI
// never sees the raw string.
export type ReviewStatus = 'queued' | 'running' | 'completed' | 'failed' | 'cancelled'
export type ExpertResultStatus = 'success' | 'warning' | 'error' | 'skipped'

/** Normalized overall risk level (from `result.consolidated.assessment`). */
export type RiskLevel = 'healthy' | 'low' | 'low-medium' | 'medium' | 'high' | 'critical'

/**
 * Lead-consolidation overall assessment, extracted from the embedded
 * `ReviewOutput` (`consolidated.assessment`) by the service layer. Absent for
 * non-completed tasks and results that predate consolidation.
 */
export interface ReviewAssessment {
  score: number
  riskLevel: RiskLevel
  unverified: boolean
}

export interface ReviewAuthor {
  name: string
  avatarUrl?: string
  // Not part of the backend contract (`author` is `{ name, avatarUrl }`);
  // retained for compatibility, currently unused.
  email?: string
}

export interface ExpertResult {
  expertId: string
  expertName: string
  status: ExpertResultStatus
  score?: number
  // Carries the curated pre-rendered Markdown report (`report.markdown` on the
  // backend; falls back to "N finding(s)" when no Markdown was produced).
  summary: string
  // Raw LLM response (`report.raw_llm_response`); debugging aid only.
  details?: string
}

export interface ReviewListItem {
  id: string
  mrTitle: string
  project: string
  repository: string
  branch: string
  targetBranch: string
  author: ReviewAuthor
  status: ReviewStatus
  durationMs: number
  createdAt: string
  gitlabMrUrl?: string
  assessment?: ReviewAssessment
}

export interface ReviewDetail {
  id: string
  mrTitle: string
  project: string
  repository: string
  branch: string
  targetBranch: string
  author: ReviewAuthor
  status: ReviewStatus
  durationMs: number
  createdAt: string
  completedAt?: string
  commitSha: string
  experts: ExpertResult[]
  rawComment?: string
  rawApiResponse?: object
  gitlabMrUrl?: string
  assessment?: ReviewAssessment
}

export interface HistoryFilters {
  q: string
  project: string | null
  status: string | null
  dateFrom: string | null
  dateTo: string | null
  repository: string | null
}

export interface HistoryState {
  reviews: ReviewListItem[]
  total: number
  page: number
  pageSize: number
  filters: HistoryFilters
  loading: boolean
  selectedReview: ReviewDetail | null
  drawerOpen: boolean
}
