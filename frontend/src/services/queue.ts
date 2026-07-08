import { request } from './api';
import type { QueueStats, QueueTask } from '../types/queue';

export interface QueueTasksResponse {
  items: QueueTask[];
  total: number;
  page: number;
  per_page: number;
}

export async function getQueueStats(): Promise<QueueStats> {
  return request('/queue/stats');
}

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

export async function cancelTask(id: string): Promise<void> {
  await request(`/queue/tasks/${id}`, { method: 'DELETE' });
}

export async function retryTask(id: string): Promise<void> {
  await request(`/queue/tasks/${id}/retry`, { method: 'POST' });
}

export async function pauseQueue(): Promise<{ status: string }> {
  return request('/queue/pause', { method: 'POST' });
}

export async function resumeQueue(): Promise<{ status: string }> {
  return request('/queue/resume', { method: 'POST' });
}

export async function setMaxConcurrent(maxConcurrent: number): Promise<{ maxConcurrent: number }> {
  return request('/queue/max-concurrent', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ max_concurrent: maxConcurrent }),
  });
}
