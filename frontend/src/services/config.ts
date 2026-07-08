import { request } from './api';
import type { AppConfig } from '../types/config';
import type { TestResult } from '../types/llm';


export async function getConfig(): Promise<AppConfig> {
  return request('/config');
}

export async function updateConfig(config: AppConfig): Promise<{ status: string }> {
  return request('/config', {
    method: 'PUT',
    body: JSON.stringify(config),
  });
}

export async function testConnection(data: {
  provider: string;
  model: string;
  apiKey: string;
  apiBase?: string;
}): Promise<TestResult> {
  return request('/config/test', {
    method: 'POST',
    body: JSON.stringify({
      provider: data.provider,
      model: data.model,
      api_key: data.apiKey,
      api_base: data.apiBase,
    }),
  });
}

export async function fetchModels(
  apiBase: string,
  apiKey: string
): Promise<{ models: string[]; error?: string }> {
  return request('/config/models', {
    method: 'POST',
    body: JSON.stringify({ api_base: apiBase, api_key: apiKey }),
  });
}
