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
   * @param silent - When true, refresh in the background: `loading` is left
   *   untouched (so the view keeps rendering the form instead of swapping in
   *   the skeleton) and a failed request keeps the last good config instead
   *   of swapping the page into its error state.
   */
  async function fetch(silent: boolean = false) {
    if (!silent) {
      loading.value = true;
      error.value = null;
    }
    try {
      config.value = await getConfig();
      // A successful background refresh clears a stale error from the first load.
      if (silent) {
        error.value = null;
      }
    } catch (e) {
      if (!silent) {
        error.value = e instanceof Error ? e.message : i18n.global.t('errors.unknown');
        config.value = null;
      }
    } finally {
      if (!silent) {
        loading.value = false;
      }
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
