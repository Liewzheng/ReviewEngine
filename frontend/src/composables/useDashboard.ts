import { ref, onMounted, onUnmounted } from 'vue';
import { getDashboard } from '../services/dashboard';
import { i18n } from '../i18n';
import type { DashboardResponse } from '../services/dashboard';

/**
 * Composable for the Dashboard page.
 *
 * Fetches dashboard KPIs on mount and auto-refreshes every 60 seconds.
 * Returns reactive `data`, `loading`, `error`, and a manual `refresh` method.
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
   * Called automatically on mount and by the refresh timer.
   */
  async function fetch() {
    loading.value = true;
    error.value = null;
    try {
      data.value = await getDashboard();
    } catch (e) {
      error.value = e instanceof Error ? e.message : i18n.global.t('errors.unknown');
    } finally {
      loading.value = false;
    }
  }

  onMounted(() => {
    fetch();
    timer = setInterval(fetch, 60000);
  });

  onUnmounted(() => {
    if (timer) clearInterval(timer);
  });

  return { data, loading, error, refresh: fetch };
}
