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
   * Populates `experts.value` on success, reconciling in place so existing
   * expert objects keep their identity across refreshes.
   * @param silent - When true, refresh in the background: `loading` is left
   *   untouched (no skeleton swap) and a failed request keeps the last good
   *   list instead of wiping it.
   */
  async function fetch(silent: boolean = false) {
    if (!silent) {
      loading.value = true;
      error.value = null;
    }
    try {
      const response = await getExperts();
      reconcileExperts(response.experts);
      if (silent) {
        error.value = null;
      }
    } catch (e) {
      if (!silent) {
        error.value = e instanceof Error ? e.message : i18n.global.t('errors.unknown');
        experts.value = [];
      }
    } finally {
      if (!silent) {
        loading.value = false;
      }
    }
  }

  /**
   * Merge a fresh fetch into the existing list instead of replacing it:
   * experts already present are mutated in place (`Object.assign`), so an
   * optimistic toggle rollback or a weight-slider drag that holds a
   * reference to an expert object is never orphaned by a background
   * refresh. New experts are appended in server order; removed ones drop out.
   */
  function reconcileExperts(fetched: Expert[]) {
    const known = new Map(experts.value.map((e) => [e.id, e]));
    experts.value = fetched.map((f) => {
      const existing = known.get(f.id);
      if (existing) {
        Object.assign(existing, f);
        return existing;
      }
      return f;
    });
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
