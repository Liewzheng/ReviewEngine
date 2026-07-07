import { ref, computed } from 'vue';
import { getProviders, testProvider } from '../services/llm';
import type { LlmProvider } from '../types/llm';

export function useLlmStatus() {
  const providers = ref<LlmProvider[]>([]);
  const loading = ref(false);
  const error = ref<string | null>(null);
  const testingId = ref<string | null>(null);

  async function fetch() {
    loading.value = true;
    error.value = null;
    try {
      const response = await getProviders();
      providers.value = response.items;
    } catch (e) {
      error.value = e instanceof Error ? e.message : 'Unknown error';
      providers.value = [];
    } finally {
      loading.value = false;
    }
  }

  async function test(id: string) {
    testingId.value = id;
    error.value = null;
    try {
      const result = await testProvider(id);
      const idx = providers.value.findIndex((p) => p.id === id);
      if (idx !== -1) {
        providers.value[idx] = {
          ...providers.value[idx],
          status: result.success ? 'healthy' : 'error',
          latencyMs: result.latencyMs ?? providers.value[idx].latencyMs,
          lastChecked: new Date().toISOString(),
        };
      }
      return result;
    } catch (e) {
      error.value = e instanceof Error ? e.message : 'Unknown error';
      throw e;
    } finally {
      testingId.value = null;
    }
  }

  const healthyCount = computed(() => providers.value.filter((p) => p.status === 'healthy').length);
  const degradedCount = computed(() => providers.value.filter((p) => p.status === 'degraded').length);
  const errorCount = computed(() => providers.value.filter((p) => p.status === 'error').length);
  const offlineCount = computed(() => providers.value.filter((p) => p.status === 'offline').length);

  return {
    providers,
    loading,
    error,
    testingId,
    healthyCount,
    degradedCount,
    errorCount,
    offlineCount,
    fetch,
    test,
  };
}
