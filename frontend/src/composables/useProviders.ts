import { ref, computed, type Ref } from 'vue';
import { ElMessageBox, ElNotification } from 'element-plus';
import { useI18n } from 'vue-i18n';
import {
  addProvider as addProviderApi,
  deleteProvider as deleteProviderApi,
  updateProvider as updateProviderApi,
  getProviders as getProvidersApi,
} from '../services/llm';
import type { ProviderEntry, ProviderConfig } from '../types/llm';

/**
 * Create a blank provider form model: `custom` type with empty fields, so the
 * Add Provider dialog starts neutral and lets the catalog picker drive any
 * auto-fill once a catalog provider is selected.
 */
export function createNewProvider(): ProviderConfig {
  return {
    provider: 'custom',
    apiKey: '',
    apiBaseUrl: '',
    defaultModel: '',
    maxTokens: 4096,
    temperature: 0.7,
    timeout: 60,
    retry: 3,
  };
}

/**
 * The backend stores temperature as f32, so the value echoed back as f64 can
 * carry precision noise (e.g. 0.30000001192092896). Display it at the edit
 * slider's 0.1-step precision.
 */
export function formatTemperature(t?: number): string {
  return t == null ? '—' : t.toFixed(1);
}

/**
 * Composable for managing additional LLM providers on the Configuration page.
 *
 * Owns the editable provider list, pending deletions, and the add/edit/delete
 * persistence orchestration against the provider CRUD endpoints. `isEditing`
 * and `configDirty` are injected from the page so the dirty flag and
 * provider-only saves stay in sync with the main configuration form.
 *
 * @param isEditing - Whether the Configuration page is in edit mode.
 * @param configDirty - Whether the main configuration form has unsaved edits.
 */
export function useProviders(isEditing: Ref<boolean>, configDirty: Ref<boolean>) {
  const { t } = useI18n();

  /** Editable list of additional providers (persisted + newly added). */
  const additionalProviders = ref<ProviderEntry[]>([]);
  /** True while the provider list is being fetched. */
  const providersLoading = ref(false);
  /** Server ids of providers removed locally, applied on the next save. */
  const deletedProviderIds = ref<string[]>([]);

  // Snapshot of the persisted provider list, used by `providersDirty` to detect
  // add/edit/delete changes. `_expanded` is pure UI state and is normalized
  // out so expanding a form never marks the page dirty.
  const originalProvidersJson = ref('');

  function serializeProviders(list: ProviderEntry[]): string {
    return JSON.stringify(list.map((p) => ({ ...p, _expanded: false })));
  }

  /** True when providers were added, edited, or marked for deletion. */
  const providersDirty = computed(() => {
    if (!isEditing.value) return false;
    if (deletedProviderIds.value.length > 0) return true;
    return serializeProviders(additionalProviders.value) !== originalProvidersJson.value;
  });

  /** Fetch the persisted provider list and snapshot it for dirty tracking. */
  async function loadProviders() {
    providersLoading.value = true;
    try {
      const resp = await getProvidersApi();
      additionalProviders.value = (resp.items || []).map((p) => ({
        provider: p.name || p.id,
        // The API key is never returned by the backend; leaving it empty
        // means "unchanged" on update (the backend keeps the stored key).
        apiKey: '',
        apiBaseUrl: p.apiBaseUrl ?? '',
        defaultModel: p.defaultModel ?? '',
        maxTokens: p.maxTokens ?? 4096,
        temperature: p.temperature ?? 0.7,
        timeout: 60,
        retry: 3,
        id: p.id,
        _key: `provider-${p.id}`,
        _expanded: false,
        _isNew: false,
      }));
    } catch {
      additionalProviders.value = [];
    } finally {
      originalProvidersJson.value = serializeProviders(additionalProviders.value);
      providersLoading.value = false;
    }
  }

  /**
   * Stage a new (not yet persisted) provider in the list. The entry is
   * expanded and flagged `_isNew` until the next save assigns a server id.
   */
  function addProvider(payload: ProviderConfig) {
    const entry: ProviderEntry = {
      ...payload,
      _key: `new-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`,
      _expanded: true,
      _isNew: true,
    };
    additionalProviders.value.push(entry);
    ElNotification({
      title: t('config.providers.addedTitle'),
      message: t('config.providers.addedMessage'),
      type: 'info',
      duration: 3000,
    });
  }

  /** Expand/collapse a provider's inline form. */
  function toggleProvider(index: number) {
    additionalProviders.value[index]._expanded = !additionalProviders.value[index]._expanded;
  }

  /** Ask for confirmation, then stage the provider for deletion on save. */
  function confirmDeleteProvider(index: number) {
    const provider = additionalProviders.value[index];
    ElMessageBox.confirm(
      t('config.providers.removeConfirm', { name: provider.provider }),
      t('config.providers.removeTitle'),
      {
        confirmButtonText: t('common.remove'),
        cancelButtonText: t('common.cancel'),
        type: 'warning',
      }
    )
      .then(() => {
        if (provider.id) {
          deletedProviderIds.value.push(provider.id);
        }
        additionalProviders.value.splice(index, 1);
      })
      .catch(() => {
        /* cancelled */
      });
  }

  /**
   * Discard unsaved provider edits, restoring the last persisted snapshot and
   * clearing pending deletions. Called when leaving edit mode without saving,
   * otherwise stale edits would keep the page dirty next time.
   */
  function resetProviders() {
    if (originalProvidersJson.value) {
      additionalProviders.value = JSON.parse(originalProvidersJson.value);
    }
    deletedProviderIds.value = [];
  }

  // The backend derives a provider's id from its list position (`{provider}-{index}`),
  // so deleting an entry renumbers every provider after it. Return the trailing
  // index portion of an id so deletes can be applied highest-index-first (each
  // id then stays valid until its own deletion).
  function providerIdIndex(id: string): number {
    const dash = id.lastIndexOf('-');
    if (dash === -1) return -1;
    const n = Number.parseInt(id.slice(dash + 1), 10);
    return Number.isNaN(n) ? -1 : n;
  }

  /**
   * Persist all pending provider add/edit/delete changes.
   * @throws If the server list drifted from the local one, or a save failed.
   */
  async function saveAdditionalProviders() {
    const hadDeletes = deletedProviderIds.value.length > 0;

    // Delete removed providers. Deleting highest index first keeps the remaining
    // ids valid — deleting a lower index first would shift the list and make the
    // higher, still-pending ids 404 (or worse, delete the wrong provider).
    const orderedDeletes = [...deletedProviderIds.value].sort(
      (a, b) => providerIdIndex(b) - providerIdIndex(a)
    );
    for (const id of orderedDeletes) {
      try {
        await deleteProviderApi(id);
      } catch (e) {
        // A 404 means the provider is already gone — e.g. PUT /config above
        // rebuilt the provider list before this loop ran. Deletion is
        // idempotent, so treat "already deleted" as success without logging.
        const message = e instanceof Error ? e.message : String(e);
        if (!message.includes('404')) {
          console.error(`Failed to delete provider ${id}`, e);
        }
      }
    }
    deletedProviderIds.value = [];

    // After any delete the server renumbers the survivors, so the ids we cached
    // before the delete would 404 on PUT below. Re-fetch and zip the remaining
    // (previously persisted, still in server order) providers onto the fresh ids.
    // Newly-added providers are excluded here — they have no server id yet and
    // are appended via POST later, so they never shift the survivors' order.
    if (hadDeletes) {
      const resp = await getProvidersApi();
      const freshItems = resp.items || [];
      const remaining = additionalProviders.value.filter((p) => p.id && !p._isNew);
      if (freshItems.length !== remaining.length) {
        // A delete failed silently or the server list changed underneath us —
        // abort rather than zip ids onto the wrong providers.
        throw new Error('Provider list changed while saving; refresh and try again');
      }
      remaining.forEach((p, i) => {
        p.id = freshItems[i].id;
      });
    }

    // Save (add or update) providers
    for (const provider of additionalProviders.value) {
      const payload: ProviderConfig = {
        provider: provider.provider,
        apiKey: provider.apiKey,
        apiBaseUrl: provider.apiBaseUrl,
        defaultModel: provider.defaultModel || undefined,
        maxTokens: provider.maxTokens,
        temperature: provider.temperature,
        timeout: provider.timeout,
        retry: provider.retry,
      };
      try {
        if (provider.id && !provider._isNew) {
          await updateProviderApi(provider.id, payload);
        } else {
          const result = await addProviderApi(payload);
          provider.id = result.id;
          provider._isNew = false;
        }
      } catch (e) {
        console.error(`Failed to save provider ${provider.provider}`, e);
        throw e;
      }
    }
    // All deletes/adds/updates succeeded — the current list is now persisted.
    originalProvidersJson.value = serializeProviders(additionalProviders.value);
  }

  /**
   * Persist only the pending provider add/edit/delete changes, leaving the
   * main config untouched. Exits edit mode when nothing else remains dirty.
   */
  async function saveProvidersOnly() {
    try {
      await saveAdditionalProviders();
      if (!configDirty.value) {
        isEditing.value = false;
      }
      ElNotification({
        title: t('common.success'),
        message: t('config.providers.saved'),
        type: 'success',
        duration: 3000,
      });
    } catch {
      ElNotification({
        title: t('common.error'),
        message: t('config.providers.saveFailed'),
        type: 'error',
        duration: 5000,
      });
    }
  }

  return {
    additionalProviders,
    providersLoading,
    providersDirty,
    loadProviders,
    addProvider,
    toggleProvider,
    confirmDeleteProvider,
    resetProviders,
    saveAdditionalProviders,
    saveProvidersOnly,
  };
}
