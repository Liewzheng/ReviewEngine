import { request } from './api';
import type { AggregatedUpdateResult, Expert, ExpertUpdateResult } from '../types/expert';

/**
 * Fetch all expert definitions from the server.
 * @returns Object containing the `experts` array (with the persisted WebUI
 *   overrides already applied server-side) plus the effective report-level
 *   `aggregated` flag (RENG-95) — the value the review paths feed
 *   `select_aggregator_expert`, surfaced so the page can show the "aggregator
 *   enabled but aggregation off" state.
 */
export async function getExperts(): Promise<{ experts: Expert[]; aggregated: boolean }> {
  return request('/system/experts');
}

/**
 * Update a single expert's configuration. The server applies it immediately
 * AND persists it, so the change survives a restart.
 *
 * @param id - Expert identifier.
 * @param data - Fields to update (enabled, weight, prompt). For `prompt`
 *   specifically (RENG-93): an empty string CLEARS the override, restoring the
 *   config file / built-in default prompt.
 * @returns The updated expert definition, plus `persisted` — `false` when the
 *   server has no store attached and the change is memory-only (lost on
 *   restart). A failed persist is an error response, so a resolved promise
 *   never means "silently dropped".
 */
export async function updateExpert(
  id: string,
  data: { enabled?: boolean; weight?: number; prompt?: string }
): Promise<ExpertUpdateResult> {
  return request(`/system/experts/${id}`, {
    method: 'PUT',
    body: JSON.stringify(data),
  });
}

/**
 * Flip the report-level aggregation flag (RENG-95). The server hot-applies it
 * to the running config AND persists it (survives a restart); the returned
 * `persisted` field says whether that write was durable.
 *
 * @param aggregated - The new flag value.
 * @returns The effective flag plus `persisted` (see {@link AggregatedUpdateResult}).
 */
export async function updateAggregated(aggregated: boolean): Promise<AggregatedUpdateResult> {
  return request('/system/experts/aggregated', {
    method: 'PUT',
    body: JSON.stringify({ aggregated }),
  });
}
