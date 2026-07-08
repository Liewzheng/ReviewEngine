import { request } from './api';
import type { LlmProvider, ProviderConfig, ProviderResponse, TestResult } from '../types/llm';

export interface LlmProvidersResponse {
  items: LlmProvider[];
}

export async function getProviders(): Promise<LlmProvidersResponse> {
  return request('/llm/providers');
}

export async function testProvider(id: string): Promise<TestResult> {
  return request(`/llm/providers/${id}/test`, { method: 'POST' });
}

/** Create a new LLM provider. */
export async function addProvider(config: ProviderConfig): Promise<ProviderResponse> {
  return request('/llm/providers', { method: 'POST', body: JSON.stringify(config) });
}

/** Delete an LLM provider by id. */
export async function deleteProvider(id: string): Promise<void> {
  return request(`/llm/providers/${id}`, { method: 'DELETE' });
}

/** Update an existing LLM provider. */
export async function updateProvider(id: string, config: Partial<ProviderConfig>): Promise<ProviderResponse> {
  return request(`/llm/providers/${id}`, { method: 'PUT', body: JSON.stringify(config) });
}
