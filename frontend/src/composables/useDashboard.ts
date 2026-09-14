import { ref, onMounted, onUnmounted } from 'vue';
import { getDashboard } from '../services/dashboard';
import { i18n } from '../i18n';
import type { DashboardResponse } from '../services/dashboard';

/**
 * Composable for the Dashboard page.
 *
 * Owns a single 60-second background poll of the dashboard KPIs; the page
 * drives the initial load through `refresh` (non-silent, so the user gets the
 * skeleton + error surfacing). The poll is silent: it never flips `loading`
 * (which gates the page's `v-if` skeleton) and a failed poll keeps the last
 * good data instead of surfacing an error. Both paths feed `lastUpdated` /
 * `pollFailed`, which the page renders as its "last updated" marker — the only
 * liveness signal left now that the manual refresh button is gone (RENG-52).
 */
export function useDashboard() {
  /** Dashboard response data (null before first load). */
  const data = ref<DashboardResponse | null>(null);
  /** True while the initial fetch is in progress. */
  const loading = ref(true);
  /** Error message if the fetch failed. */
  const error = ref<string | null>(null);
  /** ISO timestamp of the last successful fetch (initial load or poll tick). */
  const lastUpdated = ref<string | null>(null);
  /** True when the most recent fetch failed, silent polls included. */
  const pollFailed = ref(false);
  /** Auto-refresh interval handle (cleaned up on unmount). */
  let timer: ReturnType<typeof setInterval> | null = null;

  /**
   * Fetch dashboard data from the server.
   * @param silent - When true, refresh in the background: `loading` is left
   *   untouched and a failed request keeps the last good data.
   */
  async function fetch(silent: boolean = false) {
    if (!silent) {
      loading.value = true;
      error.value = null;
    }
    try {
      data.value = await getDashboard();
      lastUpdated.value = new Date().toISOString();
      pollFailed.value = false;
      if (silent) {
        error.value = null;
      }
    } catch (e) {
      // `error` stays reserved for the non-silent path (the page notifies on
      // it); a silent failure is reported through `pollFailed` only.
      pollFailed.value = true;
      if (!silent) {
        error.value = e instanceof Error ? e.message : i18n.global.t('errors.unknown');
      }
    } finally {
      if (!silent) {
        loading.value = false;
      }
    }
  }

  onMounted(() => {
    timer = setInterval(() => fetch(true), 60000);
  });

  onUnmounted(() => {
    if (timer) clearInterval(timer);
  });

  return { data, loading, error, lastUpdated, pollFailed, refresh: fetch };
}
