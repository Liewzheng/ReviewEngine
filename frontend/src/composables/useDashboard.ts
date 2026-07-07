import { ref, onMounted, onUnmounted } from 'vue';
import { getDashboard } from '../services/dashboard';
import type { DashboardResponse } from '../services/dashboard';

export function useDashboard() {
  const data = ref<DashboardResponse | null>(null);
  const loading = ref(true);
  const error = ref<string | null>(null);
  let timer: ReturnType<typeof setInterval> | null = null;

  async function fetch() {
    loading.value = true;
    error.value = null;
    try {
      data.value = await getDashboard();
    } catch (e) {
      error.value = e instanceof Error ? e.message : 'Unknown error';
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
