import { ref, computed } from 'vue';
import { getReviews, getReview, deleteReview, rerunReview } from '../services/reviews';
import { i18n } from '../i18n';
import type { ReviewsListResponse } from '../services/reviews';
import type { ReviewDetail, HistoryFilters } from '../types/history';

/**
 * Composable for the Review History page.
 *
 * Manages the paginated review list, review detail selection,
 * and review operations (delete, rerun). Also supports automatic
 * background refresh so new reviews appear without manual interaction.
 */
export function useReviews() {
  /** Paginated review list response (null before first load). */
  const data = ref<ReviewsListResponse | null>(null);
  /** Currently selected review detail (null when no review is selected). */
  const selectedReview = ref<ReviewDetail | null>(null);
  /** True while a fetch operation is in progress. */
  const loading = ref(false);
  /** Last error message. */
  const error = ref<string | null>(null);

  /**
   * Fetch a paginated list of reviews matching the given filters.
   * @param filters - Search/filter criteria (status, project, date range).
   * @param page - Page number (1-based).
   * @param perPage - Items per page.
   * @param silent - When true, refresh in the background: `loading` is left
   *   untouched (so the view keeps rendering the table instead of swapping in
   *   the skeleton) and a failed request is ignored instead of clearing the
   *   list the user is currently looking at.
   */
  async function fetchReviews(
    filters: HistoryFilters,
    page: number = 1,
    perPage: number = 20,
    silent: boolean = false
  ) {
    if (!silent) {
      loading.value = true;
      error.value = null;
    }
    try {
      const result = await getReviews(filters, page, perPage);
      data.value = result;
      // A successful background refresh clears a stale error from the first load.
      if (silent) {
        error.value = null;
      }
    } catch (e) {
      // Background polling must not wipe the list on a single transient
      // network failure; keep the last good data and skip the error banner.
      if (!silent) {
        error.value = e instanceof Error ? e.message : i18n.global.t('errors.unknown');
        data.value = null;
      }
    } finally {
      if (!silent) {
        loading.value = false;
      }
    }
  }

  /**
   * Fetch full details for a single review by ID.
   * @param id - Review UUID.
   */
  async function fetchReview(id: string) {
    loading.value = true;
    error.value = null;
    try {
      selectedReview.value = await getReview(id);
    } catch (e) {
      error.value = e instanceof Error ? e.message : i18n.global.t('errors.unknown');
      selectedReview.value = null;
    } finally {
      loading.value = false;
    }
  }

  /**
   * Delete a review by ID.
   * @param id - Review UUID to delete.
   */
  async function removeReview(id: string) {
    error.value = null;
    try {
      await deleteReview(id);
    } catch (e) {
      error.value = e instanceof Error ? e.message : i18n.global.t('errors.unknown');
      throw e;
    }
  }

  /**
   * Turn the backend's rerun rejection (404/409/422) into a message the user
   * can act on. The underlying `Error` carries `.status` from the request layer.
   */
  function rerunErrorMessage(e: unknown): string {
    const status = (e as { status?: number } | null)?.status;
    if (status === 404) {
      return i18n.global.t('errors.reviewNotFound');
    }
    if (status === 409) {
      return i18n.global.t('errors.reviewInProgress');
    }
    if (status === 422) {
      return i18n.global.t('errors.reviewParamsUnavailable');
    }
    return e instanceof Error ? e.message : i18n.global.t('errors.unknown');
  }

  /**
   * Re-run a previous review with the same parameters.
   *
   * A 422 with body code `llmNotConfigured` is surfaced by the view as a
   * "go to Configuration" dialog instead of the generic error toast, so it
   * is rethrown without populating `error`.
   * @param id - Review UUID to re-run.
   * @returns The new review task ID on success.
   */
  async function rerun(id: string) {
    error.value = null;
    try {
      return await rerunReview(id);
    } catch (e) {
      const err = e as { status?: number; code?: string } | null;
      if (err?.status !== 422 || err?.code !== 'llmNotConfigured') {
        error.value = rerunErrorMessage(e);
      }
      throw e;
    }
  }

  /* ─────────────── Auto refresh ─────────────── */
  let autoRefreshTimer: ReturnType<typeof setInterval> | null = null;
  let autoRefreshParams: { filters: HistoryFilters; page: number; perPage: number } | null = null;

  /**
   * Start polling the current filtered list every `intervalMs` milliseconds.
   * The latest filters, page and page size are captured so subsequent
   * refreshes keep the user's view state (search, filters, pagination).
   *
   * Refreshes are silent: no skeleton replaces the table and a failed poll
   * leaves the currently displayed list intact.
   *
   * Call `stopAutoRefresh` before the component unmounts.
   */
  function startAutoRefresh(filters: HistoryFilters, page: number, perPage: number, intervalMs = 5000) {
    stopAutoRefresh();
    autoRefreshParams = { filters, page, perPage };
    autoRefreshTimer = setInterval(() => {
      // Skip refreshes while the tab is hidden to avoid unnecessary load.
      if (typeof document !== 'undefined' && document.hidden) return;
      const p = autoRefreshParams;
      if (p) {
        fetchReviews(p.filters, p.page, p.perPage, true);
      }
    }, intervalMs);
  }

  /**
   * Stop the background polling started by `startAutoRefresh`.
   */
  function stopAutoRefresh() {
    if (autoRefreshTimer) {
      clearInterval(autoRefreshTimer);
      autoRefreshTimer = null;
    }
  }

  /**
   * Refresh immediately when the page regains focus, then resume the normal
   * polling cycle. The caller should register this with `visibilitychange`.
   *
   * Like the polling cycle this is a silent refresh: the table stays mounted
   * and a failure does not clear the list already on screen.
   */
  function refreshOnFocus() {
    if (typeof document === 'undefined' || document.visibilityState !== 'visible') return;
    const p = autoRefreshParams;
    if (p) {
      fetchReviews(p.filters, p.page, p.perPage, true);
    }
  }

  /** Current page of review items. */
  const items = computed(() => data.value?.items ?? []);
  /** Total number of reviews matching the current filters. */
  const total = computed(() => data.value?.total ?? 0);

  return {
    items,
    total,
    selectedReview,
    loading,
    error,
    fetchReviews,
    fetchReview,
    removeReview,
    rerun,
    startAutoRefresh,
    stopAutoRefresh,
    refreshOnFocus,
  };
}
