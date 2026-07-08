import { ref, computed } from 'vue';
import { getQueueStats, getQueueTasks, cancelTask, retryTask, pauseQueue, resumeQueue, setMaxConcurrent } from '../services/queue';
import type { QueueTasksResponse } from '../services/queue';
import type { QueueStats } from '../types/queue';

export function useQueue() {
  const stats = ref<QueueStats | null>(null);
  const data = ref<QueueTasksResponse | null>(null);
  const loadingCount = ref(0);
  const error = ref<string | null>(null);

  const loading = computed(() => loadingCount.value > 0);

  async function fetchStats() {
    loadingCount.value++;
    error.value = null;
    try {
      stats.value = await getQueueStats();
    } catch (e) {
      error.value = e instanceof Error ? e.message : 'Unknown error';
      stats.value = null;
    } finally {
      loadingCount.value--;
    }
  }

  async function fetchTasks(status?: string, page: number = 1, perPage: number = 50) {
    loadingCount.value++;
    error.value = null;
    try {
      data.value = await getQueueTasks(status, page, perPage);
    } catch (e) {
      error.value = e instanceof Error ? e.message : 'Unknown error';
      data.value = null;
    } finally {
      loadingCount.value--;
    }
  }

  async function cancel(id: string) {
    error.value = null;
    try {
      await cancelTask(id);
    } catch (e) {
      error.value = e instanceof Error ? e.message : 'Unknown error';
      throw e;
    }
  }

  async function retry(id: string) {
    error.value = null;
    try {
      await retryTask(id);
      await fetchTasks();
      await fetchStats();
    } catch (e) {
      error.value = e instanceof Error ? e.message : 'Unknown error';
      throw e;
    }
  }

  async function pause() {
    error.value = null;
    try {
      await pauseQueue();
      await fetchStats();
    } catch (e) {
      error.value = e instanceof Error ? e.message : 'Unknown error';
      throw e;
    }
  }

  async function resume() {
    error.value = null;
    try {
      await resumeQueue();
      await fetchStats();
    } catch (e) {
      error.value = e instanceof Error ? e.message : 'Unknown error';
      throw e;
    }
  }

  async function updateMaxConcurrent(value: number) {
    error.value = null;
    try {
      await setMaxConcurrent(value);
      await fetchStats();
    } catch (e) {
      error.value = e instanceof Error ? e.message : 'Unknown error';
      throw e;
    }
  }

  const isPaused = computed(() => stats.value?.isPaused ?? false);
  const items = computed(() => data.value?.items ?? []);
  const total = computed(() => data.value?.total ?? 0);

  return {
    stats,
    items,
    total,
    isPaused,
    loading,
    error,
    fetchStats,
    fetchTasks,
    cancel,
    retry,
    pause,
    resume,
    updateMaxConcurrent,
  };
}
