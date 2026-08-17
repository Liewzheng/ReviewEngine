/** Aggregate queue statistics for the dashboard. */
export interface QueueStats {
  /** Number of tasks currently running. */
  active: number
  /** Number of tasks waiting in the queue. */
  queued: number
  /** Total number of failed tasks (all time). */
  failed: number
  /** Total queue depth (active + queued). */
  totalDepth: number
  /** Maximum allowed concurrent tasks. */
  maxConcurrent: number
  /** Maximum total queue capacity. */
  queueCapacity: number
  /** Failed tasks in the last 24 hours. */
  failedLast24h: number
  /** Total tasks created in the last 24 hours. */
  totalLast24h: number
  /** Whether the queue is paused (new tasks stay pending). */
  isPaused: boolean
}

/** Lifecycle status of a queue task. */
export type TaskStatus = 'running' | 'queued' | 'failed' | 'completed' | 'cancelled';

/** A single review task in the queue. */
export interface QueueTask {
  /** Task UUID. */
  id: string
  /** MR/PR title. */
  mrTitle: string
  /** Project or namespace path. */
  project: string
  /** Repository name. */
  repository: string
  /** Current lifecycle status. */
  status: TaskStatus
  /** Completion percentage (0–100); null for queued/cancelled tasks. */
  progress: number | null
  /** Name of the expert currently executing (null if not started). */
  expertName: string | null
  /** Elapsed wall-clock time in milliseconds since task start. */
  elapsedMs: number
  /** ISO 8601 timestamp when the task was created. */
  createdAt: string
  /** ISO 8601 timestamp when the task started running. */
  startedAt?: string
  /** Error message if the task failed. */
  errorMessage?: string
}

/** Reactive state for the Queue Monitor page. */
export interface QueueState {
  /** List of tasks matching the current filter. */
  tasks: QueueTask[]
  /** Aggregate queue statistics (null before first load). */
  stats: QueueStats | null
  /** Whether the queue is paused. */
  isPaused: boolean
  /** Whether task data is being loaded. */
  loading: boolean
  /** Whether the SSE event stream is connected. */
  sseConnected: boolean
}
