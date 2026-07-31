import { ref, computed, onMounted, onUnmounted, reactive } from 'vue';
import { createLogStream, downloadLogs, fetchLogHistory } from '../services/logs';
import type { LogEntry } from '../types/logs';
import type { LogLevel } from '../types/logs';

const MAX_LOGS = 1000;

export function useLogs() {
  const logs = ref<LogEntry[]>([]);
  const loading = ref(true);
  const error = ref<string | null>(null);
  const isPaused = ref(false);
  const levels = ref<LogLevel[]>(['INFO', 'WARN', 'ERROR', 'DEBUG']);
  const keyword = ref('');
  let es: EventSource | null = null;
  let buffered: LogEntry[] = [];
  // Ids currently held in `logs`; used to drop SSE entries that overlap the
  // tail of the history backfill and to keep reconnect idempotent.
  const seenIds = new Set<string>();

  /** Append one entry, enforcing the buffer cap and deduplicating by id. */
  function pushEntry(entry: LogEntry) {
    if (entry.id && seenIds.has(entry.id)) return;
    if (entry.id) seenIds.add(entry.id);
    logs.value.push(entry);
    if (logs.value.length > MAX_LOGS) {
      const removed = logs.value.shift();
      if (removed?.id) seenIds.delete(removed.id);
    }
  }

  /** Backfill the buffer with history, oldest first, respecting the cap. */
  function loadHistory(entries: LogEntry[]) {
    const sorted = [...entries].sort(
      (a, b) => new Date(a.timestamp).getTime() - new Date(b.timestamp).getTime()
    );
    for (const entry of sorted) pushEntry(entry);
  }

  async function connect() {
    if (es) {
      es.close();
      es = null;
    }
    loading.value = true;
    error.value = null;
    try {
      const history = await fetchLogHistory();
      loadHistory(history);
    } catch (e) {
      // Backfill is best-effort; the live stream still starts below.
      console.warn('Failed to load log history:', e);
      error.value = 'Failed to load log history';
    } finally {
      loading.value = false;
    }
    es = createLogStream(
      (entry) => {
        if (isPaused.value) {
          buffered.push(entry);
        } else {
          pushEntry(entry);
        }
      },
      (err) => {
        error.value = 'SSE connection error';
        console.error('SSE error:', err);
      }
    );
  }

  function disconnect() {
    if (es) {
      es.close();
      es = null;
    }
  }

  function togglePause() {
    isPaused.value = !isPaused.value;
    if (!isPaused.value && buffered.length > 0) {
      for (const entry of buffered) pushEntry(entry);
      buffered = [];
    }
  }

  function clearLogs() {
    logs.value = [];
    buffered = [];
    seenIds.clear();
  }

  async function download() {
    error.value = null;
    try {
      const blob = await downloadLogs();
      const text = await blob.text();
      const sanitized = maskSensitive(text);
      const sanitizedBlob = new Blob([sanitized], { type: blob.type || 'application/jsonl' });
      const url = URL.createObjectURL(sanitizedBlob);
      const a = document.createElement('a');
      a.href = url;
      a.download = `logs-${new Date().toISOString()}.jsonl`;
      a.click();
      URL.revokeObjectURL(url);
    } catch (e) {
      error.value = e instanceof Error ? e.message : 'Download failed';
    }
  }

  function maskSensitive(text: string): string {
    // Mask common key-value patterns (api_key, apikey, api-key, token, secret, password)
    // and BasicAuth Bearer headers. Keep the key/prefix visible; replace the value.
    return text
      .replace(
        /((?:api[_-]?key|token|secret|password)\s*[:=]\s*)[^\n"']*/gi,
        '$1***REDACTED***'
      )
      .replace(/(Authorization:\s*Bearer\s+)[^\n]+/gi, '$1***REDACTED***');
  }

  const filteredLogs = computed(() => {
    return logs.value.filter((log) => {
      if (!levels.value.includes(log.level)) return false;
      if (keyword.value && !log.message.toLowerCase().includes(keyword.value.toLowerCase())) return false;
      return true;
    });
  });

  onMounted(connect);
  onUnmounted(disconnect);

  return reactive({
    logs,
    filteredLogs,
    loading,
    error,
    isPaused,
    levels,
    keyword,
    togglePause,
    clearLogs,
    reconnect: connect,
    download,
  });
}
