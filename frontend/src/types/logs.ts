/** Structured metadata attached to a log entry (matches backend LogMetadata). */
export interface LogMetadata {
  /** HTTP request ID that triggered this log. */
  requestId?: string
  /** Duration of the operation in milliseconds. */
  durationMs?: number
  /** Review task identifier (if the log is review-related). */
  reviewId?: string
  /** Expert identifier (if the log is expert-related). */
  expertId?: string
}

/** A single log entry from the server's in-memory ring buffer. */
export interface LogEntry {
  /** Unique log entry ID (UUID). */
  id: string
  /** ISO 8601 timestamp when the log was recorded. */
  timestamp: string
  /** Log level: INFO, WARN, ERROR, or DEBUG. */
  level: 'INFO' | 'WARN' | 'ERROR' | 'DEBUG'
  /** Human-readable log message. */
  message: string
  /** Optional structured metadata (request ID, duration, review ID). */
  metadata?: LogMetadata
}

/** Log level filter option. */
export type LogLevel = 'INFO' | 'WARN' | 'ERROR' | 'DEBUG'

/** Timestamp display format for log entries. */
export type TimestampFormat = 'relative' | 'absolute' | 'iso'

/** Reactive state for the System Logs page. */
export interface LogsState {
  /** All loaded log entries (ring buffer snapshot). */
  logs: LogEntry[]
  /** Filtered log entries (after level and keyword filters). */
  filteredLogs: LogEntry[]
  /** Active log level filters. */
  levels: LogLevel[]
  /** Search keyword filter (case-insensitive substring match). */
  keyword: string
  /** Whether to auto-scroll to the newest log entry. */
  autoScroll: boolean
  /** How timestamps are displayed. */
  timestampFormat: TimestampFormat
  /** Whether SSE streaming is paused. */
  isPaused: boolean
  /** Buffered logs waiting to be flushed when auto-scroll resumes. */
  bufferedLogs: LogEntry[]
  /** Whether the initial log load is in progress. */
  loading: boolean
  /** Count of new logs received while streaming was paused. */
  newLogCount: number
}
