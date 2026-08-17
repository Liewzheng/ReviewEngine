import { ref, computed } from 'vue';
import { getExperts, updateExpert } from '../services/experts';
import { i18n } from '../i18n';
import type { Expert } from '../types/expert';

/**
 * Composable for managing expert definitions and their configurations.
 *
 * Provides the expert list, computed helpers for enabled experts and
 * total weight, and methods to fetch/update expert settings.
 */
export function useExperts() {
  /** All expert definitions from the server. */
  const experts = ref<Expert[]>([]);
  /** True while the expert list is being fetched. */
  const loading = ref(false);
  /** Last error message. */
  const error = ref<string | null>(null);

  /**
   * Fetch the full expert list from the server.
   * Populates `experts.value` on success.
   */
  async function fetch() {
    loading.value = true;
    error.value = null;
    try {
      const response = await getExperts();
      experts.value = response.experts;
    } catch (e) {
      error.value = e instanceof Error ? e.message : i18n.global.t('errors.unknown');
      experts.value = [];
    } finally {
      loading.value = false;
    }
  }

  /**
   * Update a single expert's configuration.
   * @param id - Expert identifier.
   * @param data - Fields to update (enabled, weight).
   * @returns The updated expert definition.
   */
  async function update(id: string, data: { enabled?: boolean; weight?: number }) {
    error.value = null;
    try {
      const updated = await updateExpert(id, data);
      const idx = experts.value.findIndex((e) => e.id === id);
      if (idx !== -1) {
        experts.value[idx] = updated;
      }
      return updated;
    } catch (e) {
      error.value = e instanceof Error ? e.message : i18n.global.t('errors.unknown');
      throw e;
    }
  }

  /** Experts that are currently enabled. */
  const enabledExperts = computed(() => experts.value.filter((e) => e.enabled));
  /** Sum of weights for all enabled experts. */
  const totalWeight = computed(() => experts.value.reduce((sum, e) => sum + (e.enabled ? e.weight : 0), 0));

  return {
    experts,
    enabledExperts,
    totalWeight,
    loading,
    error,
    fetch,
    update,
  };
}
