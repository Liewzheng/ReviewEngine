import { request } from './api';
import type { LlmProvider, TestResult } from '../types/llm';

export interface LlmProvidersResponse {
  items: LlmProvider[]
  /**
   * RENG-56: length of the usage window the per-provider statistics cover
   * (rolling days, server-side `USAGE_WINDOW_DAYS`). The UI labels the
   * numbers with it so nothing shows an unlabelled window.
   */
  usageWindowDays: number
  /** RENG-56: ISO 8601 start of that window (inclusive). */
  usageSince: string
  /**
   * RENG-56: false when the server has no usage history to read
   * (`REVIEW_DISABLE_DB=1` or the aggregate failed) — every usage metric is
   * `null` then, and the page shows `—` instead of a window.
   */
  usageAvailable: boolean
  /**
   * RENG-56: total usages recorded in the window, across EVERY provider name
   * (including ones no longer configured), i.e. the denominator of each
   * card's `usageShare`. `null` when unavailable.
   */
  usageTotal: number | null
  /**
   * RENG-57: length of the latency window the per-call numbers cover (rolling
   * days, server-side `LATENCY_WINDOW_DAYS`). Reported separately from the
   * usage window so the client never assumes the two agree; both are 7 today.
   */
  latencyWindowDays: number
  /** RENG-57: ISO 8601 start of that window (inclusive). */
  latencySince: string
  /**
   * RENG-57: false when the server has no call-sample history to read — every
   * latency metric is `null` then and the page shows `—` instead of a series.
   */
  latencyAvailable: boolean
}

export async function getProviders(): Promise<LlmProvidersResponse> {
  return request('/llm/providers');
}

export async function testProvider(id: string): Promise<TestResult> {
  return request(`/llm/providers/${id}/test`, {
    method: 'POST',
    // The backend extracts a JSON body (`Json<serde_json::Value>`) for this
    // endpoint, and `request` always sets `Content-Type: application/json`
    // on POST — a bodyless request is therefore rejected with 400
    // (`EOF while parsing`). Send an explicit empty object.
    body: JSON.stringify({}),
  });
}

/**
 * Delete an LLM provider by id. Provider mutations otherwise persist through
 * the sparse `PUT /config` {llm} path; this endpoint is only needed to
 * remove the LAST provider, which the PUT rebuild cannot express (the
 * backend only replaces the runtime set when the resolved list is
 * non-empty, and a blank scalar key means "keep").
 */
export async function deleteProvider(id: string): Promise<void> {
  return request(`/llm/providers/${id}`, { method: 'DELETE' });
}
