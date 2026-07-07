import { ref, computed } from 'vue';
import { getQueueStats, getQueueTasks, cancelTask } from '../services/queue';
import type { QueueTasksResponse } from '../services/queue';
import type { QueueStats } from '../types/queue';

export function useQueue() {
  const stats = ref<QueueStats | null>(null);
  const data = ref<QueueTasksResponse | null>(null);
  const loading = ref(false);
  const error = ref<string | null>(null);

  async function fetchStats() {
    loading.value = true;
    error.value = null;
    try {
      stats.value = await getQueueStats();
    } catch (e) {
      error.value = e instanceof Error ? e.message : 'Unknown error';
      stats.value = null;
    } finally {
      loading.value = false;
    }
  }

  async function fetchTasks(status?: string, page: number = 1, perPage: number = 50) {
    loading.value = true;
    error.value = null;
    try {
      data.value = await getQueueTasks(status, page, perPage);
    } catch (e) {
      error.value = e instanceof Error ? e.message : 'Unknown error';
      data.value = null;
    } finally {
      loading.value = false;
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

  const items = computed(() => data.value?.items ?? []);
  const total = computed(() => data.value?.total ?? 0);

  return {
    stats,
    items,
    total,
    loading,
    error,
    fetchStats,
    fetchTasks,
    cancel,
  };
}
