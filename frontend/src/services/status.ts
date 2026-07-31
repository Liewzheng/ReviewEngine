import type { ReviewStatus } from '../types/history';

/**
 * Map the API's status vocabulary to the display-facing `ReviewStatus`.
 *
 * The reviews and dashboard endpoints emit `pending` for queued tasks, while
 * the UI labels the state "Queued". The queue monitor API (`/queue/tasks`)
 * already uses `queued`, so this mapping applies to reviews + dashboard only.
 */
const API_STATUS_TO_DISPLAY: Record<string, ReviewStatus> = {
  pending: 'queued',
  running: 'running',
  completed: 'completed',
  failed: 'failed',
  cancelled: 'cancelled',
};

/**
 * Normalize a raw API status string for display. Unknown/absent values pass
 * through as text rather than rendering blank; a truly absent status falls
 * back to `queued` so the label is never raw "undefined".
 */
export function normalizeStatus(status: string | null | undefined): ReviewStatus {
  if (!status) return 'queued';
  return API_STATUS_TO_DISPLAY[status] ?? (status as ReviewStatus);
}
