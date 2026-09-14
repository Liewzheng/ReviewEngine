import { ref, computed } from 'vue';
import { getProviders, testProvider } from '../services/llm';
import { i18n } from '../i18n';
import { useTransientResult } from './useTransientResult';
import type { LlmProvider, TestResult } from '../types/llm';

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
   * Last connectivity-test result per provider identity (RENG-54).
   *
   * Session state, deliberately kept OUT of `providers`: the page polls
   * `GET /llm/providers` every 30 s and `reconcileProviders` writes the
   * server's answer onto those objects in place, so a result stored there is
   * gone on the next tick — the server has no per-request latency to report
   * (it answers `latencyMs: 0` until real stats land), which is the whole
   * reason the test exists. Written only by `test()`; cleared by the card's
   * dismiss control, by an edit that invalidates the tested configuration, or
   * by leaving the page (this composable instance is page-scoped).
   *
   * Keyed by `resultKey` — the provider's NAME, which is what the config echo
   * carries as `providers[].provider` and what the page keys its cards by —
   * never by the runtime id from `GET /llm/providers`: the backend composes
   * that as `<provider>-<position>` (`src/server/api/llm.rs`), so removing
   * any provider shifts the positions of the ones behind it and a result
   * stored under the old id becomes unreachable — the line would disappear
   * from a card whose provider the user had just tested.
   */
  const testResults = useTransientResult<TestResult>();

  /**
   * Stable identity of the provider behind a runtime id.
   *
   * `LlmProvider.name` and the config echo's `providers[].provider` are the
   * same string — the page resolves a card's runtime entry with
   * `name === card.provider` — so the name survives any reindexing of the
   * runtime list. Falls back to the id when the provider is not in the list
   * (the caller only ever passes ids taken from it).
   */
  function resultKey(id: string): string {
    return providers.value.find((p) => p.id === id)?.name ?? id;
  }

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
   *
   * The outcome is recorded in `testResults` under the provider's identity
   * (`resultKey`) and is NOT merged into the provider list: that list is the
   * server's health payload (reconciled in place on every poll tick), so a
   * value written there would survive only until the next refresh (RENG-54).
   * A transport failure is recorded the same way — the test was run and it
   * failed — before the error is rethrown for the page's error notification.
   * @param id - Provider identifier to test.
   * @returns The test result with success status and latency.
   */
  async function test(id: string) {
    const key = resultKey(id);
    testingId.value = id;
    error.value = null;
    try {
      const result = await testProvider(id);
      testResults.set(key, result);
      return result;
    } catch (e) {
      const message = e instanceof Error ? e.message : i18n.global.t('errors.unknown');
      testResults.set(key, { success: false, error: message });
      error.value = message;
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
    testResults,
    healthyCount,
    degradedCount,
    errorCount,
    offlineCount,
    fetch,
    test,
  };
}
