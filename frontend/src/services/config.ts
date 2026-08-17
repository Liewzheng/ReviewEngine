import { request } from './api';
import type { AppConfig } from '../types/config';
import type { TestResult } from '../types/llm';

/**
 * Fetch the current application configuration from the server.
 * @returns The full AppConfig object.
 */
export async function getConfig(): Promise<AppConfig> {
  return request('/config');
}

/**
 * Update the application configuration on the server.
 * @param config - The new configuration to apply.
 * @returns Status confirmation on success.
 */
export async function updateConfig(config: AppConfig): Promise<{ status: string }> {
  return request('/config', {
    method: 'PUT',
    body: JSON.stringify(config),
  });
}

/**
 * Test LLM provider connectivity with the given credentials.
 * Sends a lightweight completion request to verify the API key and endpoint.
 * @param data - Provider type, model, API key, and optional base URL.
 * @returns Test result with success status and latency.
 */
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

/**
 * Fetch available model names from the given API endpoint.
 * Useful for populating model dropdowns in the configuration UI.
 * @param apiBase - The provider's API base URL.
 * @param apiKey - The provider's API key.
 * @returns Array of model identifier strings, or an error message.
 */
export async function fetchModels(
  apiBase: string,
  apiKey: string
): Promise<{ models: string[]; error?: string }> {
  return request('/config/models', {
    method: 'POST',
    body: JSON.stringify({ api_base: apiBase, api_key: apiKey }),
  });
}
