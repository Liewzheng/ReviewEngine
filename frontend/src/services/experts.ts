import { request } from './api';
import type { Expert, ExpertUpdateResult } from '../types/expert';

/**
 * Fetch all expert definitions from the server.
 * @returns Object containing the `experts` array (with the persisted WebUI
 *   overrides already applied server-side).
 */
export async function getExperts(): Promise<{ experts: Expert[] }> {
  return request('/system/experts');
}

/**
 * Update a single expert's configuration. The server applies it immediately
 * AND persists it, so the change survives a restart.
 *
 * @param id - Expert identifier.
 * @param data - Fields to update (enabled, weight).
 * @returns The updated expert definition, plus `persisted` — `false` when the
 *   server has no store attached and the change is memory-only (lost on
 *   restart). A failed persist is an error response, so a resolved promise
 *   never means "silently dropped".
 */
export async function updateExpert(
  id: string,
  data: { enabled?: boolean; weight?: number }
): Promise<ExpertUpdateResult> {
  return request(`/system/experts/${id}`, {
    method: 'PUT',
    body: JSON.stringify(data),
  });
}
