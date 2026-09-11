import { ref, onMounted, onUnmounted } from 'vue';
import { getDashboard } from '../services/dashboard';
import { i18n } from '../i18n';
import type { DashboardResponse } from '../services/dashboard';

/**
 * Composable for the Dashboard page.
 *
 * Owns a single 60-second background poll of the dashboard KPIs; the page
 * drives the initial load and manual refreshes through `refresh` (non-silent,
 * so the user gets the skeleton + error surfacing) and sets its own
 * "last updated" timestamp. The poll is silent: it never flips `loading`
 * (which gates the page's `v-if` skeleton) and a failed poll keeps the last
 * good data instead of surfacing an error.
 */
export function useDashboard() {
  /** Dashboard response data (null before first load). */
  const data = ref<DashboardResponse | null>(null);
  /** True while the initial fetch is in progress. */
  const loading = ref(true);
  /** Error message if the fetch failed. */
  const error = ref<string | null>(null);
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
      if (silent) {
        error.value = null;
      }
    } catch (e) {
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

  return { data, loading, error, refresh: fetch };
}
