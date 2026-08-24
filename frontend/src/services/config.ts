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
 *
 * The backend treats the payload as a PARTIAL (sparse) update: top-level
 * sections omitted from `config` keep their stored values, so callers may
 * send a single section (e.g. `{ llm: ... }`) without touching the rest.
 * Masked (`***`) or blank secrets keep the stored values.
 * @param config - The configuration (or section subset) to apply.
 * @returns Status confirmation on success.
 */
export async function updateConfig(config: Partial<AppConfig>): Promise<{ status: string }> {
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

/** Result of a git platform connectivity probe. */
export interface GitPlatformTestResult {
  /** Whether the probe reached the platform and authenticated. */
  ok: boolean;
  /** Platform version string when the probe succeeded. */
  version?: string;
  /** Error description when the probe failed. */
  error?: string;
}

/**
 * Probe a git platform instance (`POST /config/git-platforms/test`).
 * The endpoint always answers HTTP 200 — probe failures arrive in the body
 * as `{ ok: false, error }`. A blank or masked (`***`) token falls back
 * server-side to the stored token of the platform with the matching
 * baseUrl, so callers can pass the masked value as-is.
 * @param data - Instance base URL and (possibly masked) access token.
 */
export async function testGitPlatform(data: {
  baseUrl: string;
  token: string;
}): Promise<GitPlatformTestResult> {
  return request('/config/git-platforms/test', {
    method: 'POST',
    body: JSON.stringify({ baseUrl: data.baseUrl, token: data.token }),
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
