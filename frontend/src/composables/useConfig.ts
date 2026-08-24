import { ref } from 'vue';
import { getConfig, updateConfig } from '../services/config';
import { i18n } from '../i18n';
import type { AppConfig } from '../types/config';

/**
 * Composable for managing application configuration state.
 *
 * Provides reactive `config`, loading/error states, and methods to
 * fetch and save (sparse, section-scoped) configuration. LLM model-list
 * fetches and connection tests call `services/config` directly from the
 * provider dialog.
 */
export function useConfig() {
  /** Current application configuration (null before first load). */
  const config = ref<AppConfig | null>(null);
  /** True while the initial config fetch is in progress. */
  const loading = ref(false);
  /** True while a save operation is in progress. */
  const saving = ref(false);
  /** Last error message (null when no error). */
  const error = ref<string | null>(null);

  /**
   * Fetch the current configuration from the server.
   * Sets `config.value` on success, or `error.value` on failure.
   */
  async function fetch() {
    loading.value = true;
    error.value = null;
    try {
      config.value = await getConfig();
    } catch (e) {
      error.value = e instanceof Error ? e.message : i18n.global.t('errors.unknown');
      config.value = null;
    } finally {
      loading.value = false;
    }
  }

  /**
   * Save configuration to the server. Accepts a sparse (section-scoped)
   * payload — the backend deep-merges it over the stored config, so omitted
   * sections are preserved both server-side and in the local cache below.
   * @param updated - The configuration (or section subset) to apply.
   * @returns The server response on success.
   * @throws On failure, sets `error.value` and re-throws.
   */
  async function save(updated: Partial<AppConfig>) {
    saving.value = true;
    error.value = null;
    try {
      const result = await updateConfig(updated);
      // Shallow-merge so a sparse save doesn't discard the cached sections
      // the caller didn't send. (Save-before-load is not a real flow; when
      // nothing is cached yet, keep the payload as-is.)
      config.value = config.value
        ? { ...config.value, ...updated }
        : (updated as AppConfig);
      return result;
    } catch (e) {
      error.value = e instanceof Error ? e.message : i18n.global.t('errors.unknown');
      throw e;
    } finally {
      saving.value = false;
    }
  }

  return {
    config,
    loading,
    saving,
    error,
    fetch,
    save,
  };
}
