import { ref, computed } from 'vue';
import { getReviews, getReview, deleteReview, rerunReview } from '../services/reviews';
import type { ReviewsListResponse } from '../services/reviews';
import type { ReviewDetail, HistoryFilters } from '../types/history';

export function useReviews() {
  const data = ref<ReviewsListResponse | null>(null);
  const selectedReview = ref<ReviewDetail | null>(null);
  const loading = ref(false);
  const error = ref<string | null>(null);

  async function fetchReviews(filters: HistoryFilters, page: number = 1, perPage: number = 20) {
    loading.value = true;
    error.value = null;
    try {
      data.value = await getReviews(filters, page, perPage);
    } catch (e) {
      error.value = e instanceof Error ? e.message : 'Unknown error';
      data.value = null;
    } finally {
      loading.value = false;
    }
  }

  async function fetchReview(id: string) {
    loading.value = true;
    error.value = null;
    try {
      selectedReview.value = await getReview(id);
    } catch (e) {
      error.value = e instanceof Error ? e.message : 'Unknown error';
      selectedReview.value = null;
    } finally {
      loading.value = false;
    }
  }

  async function removeReview(id: string) {
    error.value = null;
    try {
      await deleteReview(id);
    } catch (e) {
      error.value = e instanceof Error ? e.message : 'Unknown error';
      throw e;
    }
  }

  async function rerun(id: string) {
    error.value = null;
    try {
      return await rerunReview(id);
    } catch (e) {
      error.value = e instanceof Error ? e.message : 'Unknown error';
      throw e;
    }
  }

  const items = computed(() => data.value?.items ?? []);
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
