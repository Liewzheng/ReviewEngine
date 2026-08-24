import { request } from './api';
import type { LlmProvider, TestResult } from '../types/llm';

export interface LlmProvidersResponse {
  items: LlmProvider[];
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
