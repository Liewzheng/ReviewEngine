import { ref, computed } from 'vue';
import { getQueueStats, getQueueTasks, cancelTask, retryTask, pauseQueue, resumeQueue, setMaxConcurrent } from '../services/queue';
import { i18n } from '../i18n';
import type { QueueTasksResponse } from '../services/queue';
import type { QueueStats } from '../types/queue';

/**
 * Composable for the Queue Monitor page.
 *
 * Manages queue statistics, task listing with pagination, and
 * queue control operations (pause/resume, cancel, retry, concurrency).
 */
export function useQueue() {
  /** Aggregate queue statistics (null before first load). */
  const stats = ref<QueueStats | null>(null);
  /** Paginated task list response (null before first load). */
  const data = ref<QueueTasksResponse | null>(null);
  /** Counter for tracking multiple concurrent loading operations. */
  const loadingCount = ref(0);
  /** Last error message. */
  const error = ref<string | null>(null);

  /** True when any loading operation is in progress. */
  const loading = computed(() => loadingCount.value > 0);

  /**
   * Fetch current queue statistics (active, queued, failed counts).
   */
  async function fetchStats() {
    loadingCount.value++;
    error.value = null;
    try {
      stats.value = await getQueueStats();
    } catch (e) {
      error.value = e instanceof Error ? e.message : i18n.global.t('errors.unknown');
      stats.value = null;
    } finally {
      loadingCount.value--;
    }
  }

  /**
   * Fetch a paginated list of queue tasks.
   * @param status - Filter by task status (optional).
   * @param page - Page number (1-based).
   * @param perPage - Items per page.
   */
  async function fetchTasks(status?: string, page: number = 1, perPage: number = 50) {
    loadingCount.value++;
    error.value = null;
    try {
      data.value = await getQueueTasks(status, page, perPage);
    } catch (e) {
      error.value = e instanceof Error ? e.message : i18n.global.t('errors.unknown');
      data.value = null;
    } finally {
      loadingCount.value--;
    }
  }

  /**
   * Cancel a queued or running task.
   * @param id - Task UUID to cancel.
   */
  async function cancel(id: string) {
    error.value = null;
    try {
      await cancelTask(id);
    } catch (e) {
      error.value = e instanceof Error ? e.message : i18n.global.t('errors.unknown');
      throw e;
    }
  }

  /**
   * Retry a failed task (resets to Pending state).
   * Automatically refreshes the task list and stats after retry.
   * @param id - Task UUID to retry.
   */
  async function retry(id: string) {
    error.value = null;
    try {
      await retryTask(id);
      await fetchTasks();
      await fetchStats();
    } catch (e) {
      error.value = e instanceof Error ? e.message : i18n.global.t('errors.unknown');
      throw e;
    }
  }

  /**
   * Pause the queue (new tasks stay pending, running tasks continue).
   * Refreshes stats after pausing.
   */
  async function pause() {
    error.value = null;
    try {
      await pauseQueue();
      await fetchStats();
    } catch (e) {
      error.value = e instanceof Error ? e.message : i18n.global.t('errors.unknown');
      throw e;
    }
  }

  /**
   * Resume the queue (allow new tasks to start up to max_concurrent).
   * Refreshes stats after resuming.
   */
  async function resume() {
    error.value = null;
    try {
      await resumeQueue();
      await fetchStats();
    } catch (e) {
      error.value = e instanceof Error ? e.message : i18n.global.t('errors.unknown');
      throw e;
    }
  }

  /**
   * Update the maximum number of concurrent running tasks.
   * @param value - New concurrency limit.
   */
  async function updateMaxConcurrent(value: number) {
    error.value = null;
    try {
      await setMaxConcurrent(value);
      await fetchStats();
    } catch (e) {
      error.value = e instanceof Error ? e.message : i18n.global.t('errors.unknown');
      throw e;
    }
  }

  /** Whether the queue is currently paused. */
  const isPaused = computed(() => stats.value?.isPaused ?? false);
  /** Current page of task items. */
  const items = computed(() => data.value?.items ?? []);
  /** Total number of tasks matching the current filter. */
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
