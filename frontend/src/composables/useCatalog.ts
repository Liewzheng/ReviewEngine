import { ref } from 'vue';
import { fetchCatalogProviders } from '../services/catalog';
import type { CatalogProvider } from '../types/catalog';

// The catalog rarely changes and is shared by every consumer (e.g. the Add
// Provider dialog), so it is cached at module level: the first caller triggers
// the fetch and later callers reuse the result. A failure is remembered so the
// UI can fall back to the built-in preset list instead of re-hammering an
// unavailable endpoint on every render.
const catalogProviders = ref<CatalogProvider[]>([]);
const catalogLoading = ref(false);
const catalogLoaded = ref(false);
const catalogFailed = ref(false);

/**
 * Shared access to the models.dev provider catalog.
 *
 * Call {@link loadCatalogProviders} when a catalog-aware UI opens, then check
 * `catalogFailed` to decide between catalog options and the preset fallback.
 */
export function useCatalog() {
  /**
   * Fetch the provider catalog once (no-op after the first attempt, successful
   * or not, unless `force` is set — pass it to retry after a transient 503).
   */
  async function loadCatalogProviders(force = false): Promise<void> {
    if (catalogLoading.value) return;
    if (catalogLoaded.value && !force) return;
    catalogLoading.value = true;
    try {
      const resp = await fetchCatalogProviders();
      catalogProviders.value = resp.providers ?? [];
      catalogFailed.value = false;
    } catch {
      catalogProviders.value = [];
      catalogFailed.value = true;
    } finally {
      catalogLoaded.value = true;
      catalogLoading.value = false;
    }
  }

  return {
    /** Catalog providers (empty until loaded, and after a failed fetch). */
    catalogProviders,
    /** True while the catalog request is in flight. */
    catalogLoading,
    /** True once the first fetch attempt has settled (success or failure). */
    catalogLoaded,
    /** True when the catalog fetch failed — fall back to the preset list. */
    catalogFailed,
    loadCatalogProviders,
  };
}
