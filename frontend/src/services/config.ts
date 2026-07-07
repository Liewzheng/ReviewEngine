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
}): Promise<TestResult> {
  return request('/config/test', {
    method: 'POST',
    body: JSON.stringify(data),
  });
}
