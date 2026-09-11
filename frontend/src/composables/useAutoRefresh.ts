/**
 * Generic background auto-refresh polling composable.
 *
 * Extracted from the ReviewHistory page's polling (RENG-40, see
 * `useReviews.startAutoRefresh`): setInterval-driven `fetchFn` calls that
 * pause while the tab is hidden and fire immediately when the user returns.
 * Refactor of useReviews onto this composable keeps that page working, and it
 * now backs the Configuration / Experts / Queue / LLM Status pages too.
 *
 * The composable only manages the timer + visibility listener; the caller
 * wires lifecycle explicitly:
 *
 * @example
 * const autoRefresh = useAutoRefresh(() => store.fetch(), 10_000)
 * onMounted(() => autoRefresh.start())
 * onBeforeUnmount(() => autoRefresh.stop())
 */
export function useAutoRefresh(fetchFn: () => Promise<void> | void, intervalMs = 5000) {
  let timer: ReturnType<typeof setInterval> | null = null;
  let inFlight = false;

  /**
   * One poll tick: skipped while the tab is hidden or a previous tick is
   * still running (prevents overlapping fetches on a slow network). Errors
   * are swallowed — views keep their last good state and surface their own
   * error handling.
   */
  async function tick(): Promise<void> {
    if (typeof document !== 'undefined' && document.hidden) return;
    if (inFlight) return;
    inFlight = true;
    try {
      await fetchFn();
    } catch {
      /* keep last good state */
    } finally {
      inFlight = false;
    }
  }

  /** Immediate refresh, e.g. when the tab regains visibility. */
  function refreshOnVisible(): void {
    if (typeof document === 'undefined' || document.visibilityState !== 'visible') return;
    tick();
  }

  function handleVisibilityChange(): void {
    if (typeof document !== 'undefined' && document.visibilityState === 'visible') {
      tick();
    }
  }

  /**
   * (Re)start polling every `ms` milliseconds. Idempotent: any running timer
   * is cleared first, so re-starting with new parameters is safe. Registers
   * the visibility listener so returning to the tab refreshes immediately.
   */
  function start(ms?: number): void {
    stop();
    timer = setInterval(tick, ms ?? intervalMs);
    if (typeof document !== 'undefined') {
      document.addEventListener('visibilitychange', handleVisibilityChange);
    }
  }

  /** Stop polling and remove the visibility listener. */
  function stop(): void {
    if (timer) {
      clearInterval(timer);
      timer = null;
    }
    if (typeof document !== 'undefined') {
      document.removeEventListener('visibilitychange', handleVisibilityChange);
    }
  }

  return { start, stop, refreshOnVisible };
}
