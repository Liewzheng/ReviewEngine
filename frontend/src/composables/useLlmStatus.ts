import { ref, computed } from 'vue';
import { getProviders, testProvider } from '../services/llm';
import { i18n } from '../i18n';
import type { LlmProvider } from '../types/llm';

/**
 * Composable for the LLM Status page.
 *
 * Manages the list of LLM providers, their health status, and
 * provides a test method to check individual provider connectivity.
 */
export function useLlmStatus() {
  /** All configured LLM providers. */
  const providers = ref<LlmProvider[]>([]);
  /** True while the provider list is being fetched. */
  const loading = ref(false);
  /** Last error message. */
  const error = ref<string | null>(null);
  /** ID of the provider currently being tested (null when idle). */
  const testingId = ref<string | null>(null);

  /**
   * Fetch the full provider list from the server.
   * Populates `providers.value` on success.
   */
  async function fetch() {
    loading.value = true;
    error.value = null;
    try {
      const response = await getProviders();
      providers.value = response.items;
    } catch (e) {
      error.value = e instanceof Error ? e.message : i18n.global.t('errors.unknown');
      providers.value = [];
    } finally {
      loading.value = false;
    }
  }

  /**
   * Test connectivity for a specific provider.
   * Updates the provider's status and latency in-place.
   * @param id - Provider identifier to test.
   * @returns The test result with success status and latency.
   */
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
      error.value = e instanceof Error ? e.message : i18n.global.t('errors.unknown');
      throw e;
    } finally {
      testingId.value = null;
    }
  }

  /** Count of providers with healthy status. */
  const healthyCount = computed(() => providers.value.filter((p) => p.status === 'healthy').length);
  /** Count of providers with degraded status. */
  const degradedCount = computed(() => providers.value.filter((p) => p.status === 'degraded').length);
  /** Count of providers with error status. */
  const errorCount = computed(() => providers.value.filter((p) => p.status === 'error').length);
  /** Count of providers that are offline. */
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
