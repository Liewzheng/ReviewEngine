import { ref } from 'vue';
import { getConfig, updateConfig, testConnection, fetchModels as fetchModelsApi } from '../services/config';
import { i18n } from '../i18n';
import type { AppConfig } from '../types/config';
import type { TestResult } from '../types/llm';

/**
 * Composable for managing application configuration state.
 *
 * Provides reactive `config`, loading/error states, and methods to
 * fetch, save, test connections, and list available models.
 */
export function useConfig() {
  /** Current application configuration (null before first load). */
  const config = ref<AppConfig | null>(null);
  /** True while the initial config fetch is in progress. */
  const loading = ref(false);
  /** True while a save operation is in progress. */
  const saving = ref(false);
  /** Last error message (null when no error). */
  const error = ref<string | null>(null);
  /** Result of the last connection test (null before first test). */
  const testResult = ref<TestResult | null>(null);
  /** True while a connection test is in progress. */
  const testing = ref(false);
  /** True while fetching available models from the provider. */
  const modelsLoading = ref(false);
  /** Error message from the last model fetch attempt. */
  const modelsError = ref<string | null>(null);

  /**
   * Fetch the current configuration from the server.
   * Sets `config.value` on success, or `error.value` on failure.
   */
  async function fetch() {
    loading.value = true;
    error.value = null;
    try {
      config.value = await getConfig();
    } catch (e) {
      error.value = e instanceof Error ? e.message : i18n.global.t('errors.unknown');
      config.value = null;
    } finally {
      loading.value = false;
    }
  }

  /**
   * Save updated configuration to the server.
   * @param updated - The new configuration to apply.
   * @returns The server response on success.
   * @throws On failure, sets `error.value` and re-throws.
   */
  async function save(updated: AppConfig) {
    saving.value = true;
    error.value = null;
    try {
      const result = await updateConfig(updated);
      config.value = updated;
      return result;
    } catch (e) {
      error.value = e instanceof Error ? e.message : i18n.global.t('errors.unknown');
      throw e;
    } finally {
      saving.value = false;
    }
  }

  /**
   * Test LLM provider connectivity with the given credentials.
   * @param data - Provider type, model, API key, and optional base URL.
   * @returns Test result with success status and latency.
   */
  async function test(data: { provider: string; model: string; apiKey: string; apiBase?: string }) {
    testing.value = true;
    error.value = null;
    testResult.value = null;
    try {
      testResult.value = await testConnection(data);
    } catch (e) {
      error.value = e instanceof Error ? e.message : i18n.global.t('errors.unknown');
      testResult.value = { success: false, error: error.value ?? undefined, timestamp: new Date().toISOString() };
    } finally {
      testing.value = false;
    }
  }

  /**
   * Fetch available model names from the given API endpoint.
   * @param apiBase - The provider's API base URL.
   * @param apiKey - The provider's API key.
   * @returns Array of model identifier strings.
   */
  async function fetchModels(apiBase: string, apiKey: string): Promise<string[]> {
    modelsLoading.value = true;
    modelsError.value = null;
    try {
      const response = await fetchModelsApi(apiBase, apiKey);
      if (response.error) {
        modelsError.value = response.error;
        return [];
      }
      return response.models || [];
    } catch (e) {
      modelsError.value = e instanceof Error ? e.message : i18n.global.t('errors.failedToFetchModels');
      return [];
    } finally {
      modelsLoading.value = false;
    }
  }

  return {
    config,
    loading,
    saving,
    error,
    testResult,
    testing,
    modelsLoading,
    modelsError,
    fetch,
    save,
    test,
    fetchModels,
  };
}
