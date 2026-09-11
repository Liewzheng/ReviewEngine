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
   * Populates `providers.value` on success, reconciling in place so existing
   * provider objects keep their identity across refreshes.
   * @param silent - When true, refresh in the background: `loading` is left
   *   untouched and a failed request keeps the last good list instead of
   *   wiping it.
   */
  async function fetch(silent: boolean = false) {
    if (!silent) {
      loading.value = true;
      error.value = null;
    }
    try {
      const response = await getProviders();
      reconcileProviders(response.items);
      if (silent) {
        error.value = null;
      }
    } catch (e) {
      if (!silent) {
        error.value = e instanceof Error ? e.message : i18n.global.t('errors.unknown');
        providers.value = [];
      }
    } finally {
      if (!silent) {
        loading.value = false;
      }
    }
  }

  /**
   * Merge a fresh fetch into the existing list instead of replacing it:
   * providers already present are mutated in place, so a connectivity
   * `test()` that captured an element reference is never orphaned by a
   * background refresh. New providers are appended in server order.
   */
  function reconcileProviders(fetched: LlmProvider[]) {
    const known = new Map(providers.value.map((p) => [p.id, p]));
    providers.value = fetched.map((f) => {
      const existing = known.get(f.id);
      if (existing) {
        Object.assign(existing, f);
        return existing;
      }
      return f;
    });
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
