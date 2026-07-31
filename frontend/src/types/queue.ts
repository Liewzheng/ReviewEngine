export interface QueueStats {
  active: number;
  queued: number;
  failed: number;
  totalDepth: number;
  maxConcurrent: number;
  queueCapacity: number;
  failedLast24h: number;
  totalLast24h: number;
  isPaused: boolean;
}

export type TaskStatus = 'running' | 'queued' | 'failed' | 'completed' | 'cancelled';

export interface QueueTask {
  id: string;
  mrTitle: string;
  project: string;
  repository: string;
  status: TaskStatus;
  // The queue API sends `null` for tasks that have not started (queued/cancelled).
  progress: number | null;
  expertName: string | null;
  elapsedMs: number;
  createdAt: string;
  startedAt?: string;
  errorMessage?: string;
}

export interface QueueState {
  tasks: QueueTask[];
  stats: QueueStats | null;
  isPaused: boolean;
  loading: boolean;
  sseConnected: boolean;
}
