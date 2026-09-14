import { ref } from 'vue';

/** One recorded outcome of a user-triggered one-off operation. */
export interface TransientResult<T> {
  /** The server's answer for the operation. */
  value: T;
  /** ISO 8601 timestamp of when the operation finished. */
  at: string;
}

/**
 * Session state for the result of a one-off operation the user triggered —
 * a connectivity test — on a page that also polls the server in the
 * background (RENG-54).
 *
 * The result must NOT live inside the polled payload, nor be merged onto
 * objects derived from it. A background tick fetches the server's own
 * truth and reconciles it onto that structure in place (the RENG-41
 * silent-poll design), and the server has no notion of the probe the user
 * just ran — `GET /llm/providers` reports `latencyMs: 0` and the config
 * echo knows nothing about a test — so the next tick overwrites the value
 * the user is still reading.
 *
 * Entries are keyed by the operation's subject (a provider id, a platform
 * name) and are written ONLY by the operation path. They are removed
 * explicitly: the row's dismiss control, a re-run overwriting its own key,
 * an edit invalidating the configuration that was tested, or the page
 * unmounting (the composable instance is page-scoped, so leaving the page
 * ends the session).
 */
export function useTransientResult<T>() {
  /** Recorded outcomes by subject key; absent = never tested this session. */
  const results = ref<Record<string, TransientResult<T>>>({});

  /** Record the outcome of the operation that just finished for `key`. */
  function set(key: string, value: T): void {
    results.value = { ...results.value, [key]: { value, at: new Date().toISOString() } };
  }

  /** The last outcome recorded for `key`, or null when there is none. */
  function get(key: string): TransientResult<T> | null {
    return results.value[key] ?? null;
  }

  /** Forget the outcome recorded for `key` (no-op when there is none). */
  function clear(key: string): void {
    if (!(key in results.value)) return;
    const next = { ...results.value };
    delete next[key];
    results.value = next;
  }

  /** Forget every recorded outcome. */
  function clearAll(): void {
    results.value = {};
  }

  return { results, set, get, clear, clearAll };
}
