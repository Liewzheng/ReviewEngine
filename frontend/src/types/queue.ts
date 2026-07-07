export interface QueueStats {
  active: number;
  queued: number;
  failed: number;
  totalDepth: number;
  maxConcurrent: number;
  queueCapacity: number;
  failedLast24h: number;
  totalLast24h: number;
}

export type TaskStatus = 'running' | 'queued' | 'failed' | 'completed';

export interface QueueTask {
  id: string;
  mrTitle: string;
  project: string;
  repository: string;
  status: TaskStatus;
  progress: number;
  expertName: string;
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
