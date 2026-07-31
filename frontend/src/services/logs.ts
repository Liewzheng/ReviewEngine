import type { LogEntry } from '../types/logs';
import { getApiToken } from './api';

export type { LogEntry };

export function createLogStream(
  onMessage: (entry: LogEntry) => void,
  onError?: (err: Event) => void
): EventSource {
  const es = new EventSource('/api/v1/logs');
  es.onmessage = (event) => {
    try {
      const data = JSON.parse(event.data);
      onMessage(data);
    } catch {
      // ignore malformed
    }
  };
  if (onError) es.onerror = onError;
  return es;
}

export async function downloadLogs(): Promise<Blob> {
  const headers: Record<string, string> = {};
  const token = getApiToken();
  if (token) {
    headers['Authorization'] = `Bearer ${token}`;
  }

  const resp = await fetch('/api/v1/logs/download', { headers });
  if (!resp.ok) throw new Error('Download failed');
  return resp.blob();
}

/**
 * Fetch the buffered log history (NDJSON, up to the backend's 1000-entry ring
 * buffer) and parse it into `LogEntry[]`. There is no paginated history API, so
 * the download endpoint doubles as the initial backfill source.
 */
export async function fetchLogHistory(): Promise<LogEntry[]> {
  const headers: Record<string, string> = {};
  const token = getApiToken();
  if (token) {
    headers['Authorization'] = `Bearer ${token}`;
  }

  const resp = await fetch('/api/v1/logs/download', { headers });
  if (!resp.ok) throw new Error('Failed to load log history');
  const text = await resp.text();

  const entries: LogEntry[] = [];
  for (const line of text.split('\n')) {
    if (!line.trim()) continue;
    try {
      entries.push(JSON.parse(line) as LogEntry);
    } catch {
      // skip malformed lines; the live SSE stream still covers new entries
    }
  }
  return entries;
}
