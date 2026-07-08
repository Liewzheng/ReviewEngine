import { ref } from 'vue';
import { getConfig, updateConfig, testConnection, fetchModels as fetchModelsApi } from '../services/config';
import type { AppConfig } from '../types/config';
import type { TestResult } from '../types/llm';

export function useConfig() {
  const config = ref<AppConfig | null>(null);
  const loading = ref(false);
  const saving = ref(false);
  const error = ref<string | null>(null);
  const testResult = ref<TestResult | null>(null);
  const testing = ref(false);
  const modelsLoading = ref(false);
  const modelsError = ref<string | null>(null);

  async function fetch() {
    loading.value = true;
    error.value = null;
    try {
      config.value = await getConfig();
    } catch (e) {
      error.value = e instanceof Error ? e.message : 'Unknown error';
      config.value = null;
    } finally {
      loading.value = false;
    }
  }

  async function save(updated: AppConfig) {
    saving.value = true;
    error.value = null;
    try {
      const result = await updateConfig(updated);
      config.value = updated;
      return result;
    } catch (e) {
      error.value = e instanceof Error ? e.message : 'Unknown error';
      throw e;
    } finally {
      saving.value = false;
    }
  }

  async function test(data: { provider: string; model: string; apiKey: string; apiBase?: string }) {
    testing.value = true;
    error.value = null;
    testResult.value = null;
    try {
      testResult.value = await testConnection(data);
    } catch (e) {
      error.value = e instanceof Error ? e.message : 'Unknown error';
      testResult.value = { success: false, error: error.value ?? undefined, timestamp: new Date().toISOString() };
    } finally {
      testing.value = false;
    }
  }

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
      modelsError.value = e instanceof Error ? e.message : 'Failed to fetch models';
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
