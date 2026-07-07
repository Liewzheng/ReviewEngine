import { request } from './api';
import type { LlmProvider } from '../types/llm';
import type { TestResult } from '../types/llm';

export interface LlmProvidersResponse {
  items: LlmProvider[];
}

export async function getProviders(): Promise<LlmProvidersResponse> {
  return request('/llm/providers');
}

export async function testProvider(id: string): Promise<TestResult> {
  return request(`/llm/providers/${id}/test`, { method: 'POST' });
}
