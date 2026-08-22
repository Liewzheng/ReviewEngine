import { ref, computed } from 'vue';
import { getReviews, getReview, deleteReview, rerunReview } from '../services/reviews';
import { i18n } from '../i18n';
import type { ReviewsListResponse } from '../services/reviews';
import type { ReviewDetail, HistoryFilters } from '../types/history';

/**
 * Composable for the Review History page.
 *
 * Manages the paginated review list, review detail selection,
 * and review operations (delete, rerun).
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
   */
  async function fetchReviews(filters: HistoryFilters, page: number = 1, perPage: number = 20) {
    loading.value = true;
    error.value = null;
    try {
      data.value = await getReviews(filters, page, perPage);
    } catch (e) {
      error.value = e instanceof Error ? e.message : i18n.global.t('errors.unknown');
      data.value = null;
    } finally {
      loading.value = false;
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
  };
}
