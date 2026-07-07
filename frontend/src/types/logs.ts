export interface LogMetadata {
  requestId?: string
  durationMs?: number
  reviewId?: string
  expertId?: string
}

export interface LogEntry {
  id: string
  timestamp: string
  level: 'INFO' | 'WARN' | 'ERROR' | 'DEBUG'
  message: string
  metadata?: LogMetadata
}

export type LogLevel = 'INFO' | 'WARN' | 'ERROR' | 'DEBUG'

export type TimestampFormat = 'relative' | 'absolute' | 'iso'

export interface LogsState {
  logs: LogEntry[]
  filteredLogs: LogEntry[]
  levels: LogLevel[]
  keyword: string
  autoScroll: boolean
  timestampFormat: TimestampFormat
  isPaused: boolean
  bufferedLogs: LogEntry[]
  loading: boolean
  newLogCount: number
}
