import type { LogEntry } from '../types/logs';

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
  const resp = await fetch('/api/v1/logs/download');
  if (!resp.ok) throw new Error('Download failed');
  return resp.blob();
}
