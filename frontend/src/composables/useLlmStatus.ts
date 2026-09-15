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
   * RENG-56: usage window the per-provider statistics cover, as reported by
   * the server with the list (`usageWindowDays` / `usageSince` /
   * `usageAvailable`). Kept alongside the providers because the numbers are
   * meaningless without it: the page labels "过去 7 天" from here, and hides
   * the usage row entirely when the server has no history to read.
   */
  const usageWindowDays = ref<number | null>(null);
  const usageSince = ref<string | null>(null);
  const usageAvailable = ref(false);
  /**
   * RENG-56: every usage recorded in the window, across all provider names —
   * the share denominator the cards are a breakdown of. `null` when the
   * server could not read the history.
   */
  const usageTotal = ref<number | null>(null);
  /**
   * RENG-57: latency window the per-call numbers cover, and whether any
   * sample history could be read at all. Same contract as the usage window:
   * the numbers are meaningless without the window, and `latencyAvailable`
   * false means every latency metric is `null` (the page shows `—`), not 0.
   */
  const latencyWindowDays = ref<number | null>(null);
  const latencySince = ref<string | null>(null);
  const latencyAvailable = ref(false);

  /**
   * Last connectivity-test result per provider identity (RENG-54).
   *
   * Session state, deliberately kept OUT of `providers`: the page polls
   * `GET /llm/providers` every 30 s and `reconcileProviders` writes the
   * server's answer onto those objects in place, so a result stored there is
   * gone on the next tick — what the poll carries is the server's own health
   * probe (`lastProbeLatencyMs` is that probe's round-trip time, RENG-36), not
   * the measurement of the call the user just made, which is the whole reason
   * the test exists. Written only by `test()`; cleared by the card's dismiss
   * control, by an edit that invalidates the tested configuration, or by
   * leaving the page (this composable instance is page-scoped).
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
   * The config echo's `providers[].provider` and the health payload's `name`
   * are the same string — but RENG-75 made that string a DISPLAY LABEL, and
   * two cards may share it (two accounts, or one account × two models), so it
   * cannot key a result on its own. The key is the visible
   * `(name, apiBaseUrl, defaultModel)` triple, which the page joins the two
   * lists on as well; it survives any reindexing of the runtime list, which
   * the server composes as `<provider>-<position>`. Falls back to the id when
   * the provider is not in the list (the caller only ever passes ids taken
   * from it) or when the payload predates the base/model echo.
   */
  function resultKey(id: string): string {
    const provider = providers.value.find((p) => p.id === id);
    if (!provider) return id;
    const triple = [provider.name, provider.apiBaseUrl ?? '', provider.defaultModel ?? ''].join('\u0000');
    return triple === '\u0000\u0000' ? id : triple;
  }

  /**
   * Deterministic provider surface for visual work (`VITE_USE_LLM_MOCKS`).
   * The flag is a build-time constant, so in a normal build this branch and
   * the dynamic import behind it are dropped entirely — mock data can never
   * reach a real deployment.
   * @returns True when the mock list was installed.
   */
  async function loadMockProviders(): Promise<boolean> {
    if (import.meta.env.VITE_USE_LLM_MOCKS !== 'true') return false;
    const { MOCK_PROVIDERS } = await import('../dev-mocks/llm-providers.mock');
    reconcileProviders(MOCK_PROVIDERS.map((p) => ({ ...p })));
    usageWindowDays.value = 7;
    usageSince.value = null;
    usageAvailable.value = true;
    usageTotal.value = MOCK_PROVIDERS.reduce((sum, p) => sum + (p.requestCount ?? 0), 0);
    latencyWindowDays.value = 7;
    latencySince.value = null;
    latencyAvailable.value = true;
    return true;
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
      if (await loadMockProviders()) return;
      const response = await getProviders();
      reconcileProviders(response.items);      // RENG-56: the window travels with the numbers it describes, and so
      // does the window total the per-provider shares are taken against.
      usageWindowDays.value = response.usageWindowDays ?? null;
      usageSince.value = response.usageSince ?? null;
      usageAvailable.value = response.usageAvailable === true;
      usageTotal.value = response.usageTotal ?? null;
      // RENG-57: same for the recorded latency, which is a different
      // measurement over its own (separately reported) window.
      latencyWindowDays.value = response.latencyWindowDays ?? null;
      latencySince.value = response.latencySince ?? null;
      latencyAvailable.value = response.latencyAvailable === true;
      if (silent) {
        error.value = null;
      }
    } catch (e) {
      if (!silent) {
        error.value = e instanceof Error ? e.message : i18n.global.t('errors.unknown');
        providers.value = [];
        // No list means no usage or latency to show either: report unknown,
        // not stale.
        usageAvailable.value = false;
        usageTotal.value = null;
        latencyAvailable.value = false;
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
    usageWindowDays,
    usageSince,
    usageAvailable,
    usageTotal,
    latencyWindowDays,
    latencySince,
    latencyAvailable,
    healthyCount,
    degradedCount,
    errorCount,
    offlineCount,
    fetch,
    test,
  };
}
