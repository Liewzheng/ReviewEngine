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
 *
 * A tick is also skipped while `options.isPaused` reports true — pages use
 * this to hold polling while transient UI state is up (an open dialog or
 * drawer, an in-flight optimistic edit) so a fetch can never disturb it.
 */
export interface AutoRefreshOptions {
  /**
   * Optional gate consulted before every tick. Return true to skip the tick.
   * Read fresh on each tick, so it may close over refs, maps, or component
   * state that changes between ticks.
   */
  isPaused?: () => boolean;
}

export function useAutoRefresh(
  fetchFn: () => Promise<void> | void,
  intervalMs = 5000,
  options: AutoRefreshOptions = {},
) {
  let timer: ReturnType<typeof setInterval> | null = null;
  let inFlight = false;

  /**
   * One poll tick: skipped while the tab is hidden, while the caller's
   * `isPaused` gate reports transient UI state (open dialog/drawer, in-flight
   * edit), or while a previous tick is still running (prevents overlapping
   * fetches on a slow network). Errors are swallowed — views keep their last
   * good state and surface their own error handling — but logged so a failing
   * poll leaves a trace.
   */
  async function tick(): Promise<void> {
    if (typeof document !== 'undefined' && document.hidden) return;
    if (options.isPaused?.()) return;
    if (inFlight) return;
    inFlight = true;
    try {
      await fetchFn();
    } catch (err) {
      console.warn('[auto-refresh] poll failed', err);
    } finally {
      inFlight = false;
    }
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

  return { start, stop };
}
