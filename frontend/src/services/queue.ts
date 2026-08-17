import { request } from './api';
import type { QueueStats, QueueTask } from '../types/queue';

/** Paginated response from the queue tasks endpoint. */
export interface QueueTasksResponse {
  /** List of task entries for the current page. */
  items: QueueTask[];
  /** Total number of tasks matching the filter. */
  total: number;
  /** Current page number (1-based). */
  page: number;
  /** Number of items per page. */
  per_page: number;
}

/**
 * Fetch aggregate queue statistics (active, queued, failed counts).
 * @returns Current queue health snapshot.
 */
export async function getQueueStats(): Promise<QueueStats> {
  return request('/queue/stats');
}

/**
 * Fetch a paginated list of queue tasks with optional filtering.
 * @param status - Filter by task status (optional).
 * @param page - Page number (1-based, default 1).
 * @param perPage - Items per page (default 50).
 * @returns Paginated task list with total count.
 */
export async function getQueueTasks(
  status?: string,
  page: number = 1,
  perPage: number = 50
): Promise<QueueTasksResponse> {
  const params = new URLSearchParams();
  if (status) params.append('status', status);
  params.append('page', String(page));
  params.append('per_page', String(perPage));
  return request(`/queue/tasks?${params.toString()}`);
}

/**
 * Cancel a queued or running task by ID.
 * The task transitions to `cancelled` state (kept in history).
 * @param id - Task UUID to cancel.
 */
export async function cancelTask(id: string): Promise<void> {
  await request(`/queue/tasks/${id}`, { method: 'DELETE' });
}

/**
 * Retry a failed task (resets to Pending state for re-execution).
 * @param id - Task UUID to retry.
 */
export async function retryTask(id: string): Promise<void> {
  await request(`/queue/tasks/${id}/retry`, { method: 'POST' });
}

/**
 * Pause the review queue. New tasks remain pending; running tasks continue.
 * @returns Status confirmation.
 */
export async function pauseQueue(): Promise<{ status: string }> {
  return request('/queue/pause', { method: 'POST' });
}

/**
 * Resume the review queue. New tasks can start up to max_concurrent.
 * @returns Status confirmation.
 */
export async function resumeQueue(): Promise<{ status: string }> {
  return request('/queue/resume', { method: 'POST' });
}

/**
 * Update the maximum number of concurrent running tasks.
 * @param maxConcurrent - New concurrency limit.
 * @returns The updated concurrency setting.
 */
export async function setMaxConcurrent(maxConcurrent: number): Promise<{ maxConcurrent: number }> {
  return request('/queue/max-concurrent', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ max_concurrent: maxConcurrent }),
  });
}
