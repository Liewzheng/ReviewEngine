import { ref, computed } from 'vue';
import { getExperts, updateExpert } from '../services/experts';
import type { Expert } from '../types/expert';

export function useExperts() {
  const experts = ref<Expert[]>([]);
  const loading = ref(false);
  const error = ref<string | null>(null);

  async function fetch() {
    loading.value = true;
    error.value = null;
    try {
      const response = await getExperts();
      experts.value = response.experts;
    } catch (e) {
      error.value = e instanceof Error ? e.message : 'Unknown error';
      experts.value = [];
    } finally {
      loading.value = false;
    }
  }

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
      error.value = e instanceof Error ? e.message : 'Unknown error';
      throw e;
    }
  }

  const enabledExperts = computed(() => experts.value.filter((e) => e.enabled));
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
