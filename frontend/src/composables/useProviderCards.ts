import { ref, type Ref } from 'vue';
import { ElMessage, ElMessageBox } from 'element-plus';
import { useI18n } from 'vue-i18n';
import { useConfig } from './useConfig';
import { deleteProvider as deleteProviderApi } from '../services/llm';
import {
  buildLlmPayload,
  cardsFromLlmConfig,
  providerDisplayName,
  type ProviderCardState,
} from './llmPayload';
import type { LlmProvider } from '../types/llm';

/**
 * Unified provider-card model for the /llm page configuration section.
 *
 * One card per configured provider (the primary included), driven by the
 * GET /config `llm` echo. Every add/edit/delete/set-primary mutation is
 * persisted immediately as a sparse `PUT {llm}` (hot-applied + written
 * through) with ElMessage feedback — there is no page-level edit mode.
 *
 * @param statusProviders - Runtime provider list (GET /llm/providers); used
 *   to resolve the server id of the final provider for the CRUD delete that
 *   PUT /config cannot express.
 * @param afterSave - Runs after every successful mutation (refresh the
 *   health cards and the not-configured banner).
 */
export function useProviderCards(options: {
  statusProviders: Ref<LlmProvider[]>;
  afterSave?: () => void;
}) {
  const { t } = useI18n();
  const cfg = useConfig();

  /** The configured providers as cards, in echo order. */
  const cards = ref<ProviderCardState[]>([]);
  /** Provider name of the primary card ('' when nothing is configured). */
  const primaryName = ref('');
  const loading = cfg.loading;
  const saving = cfg.saving;
  const error = cfg.error;

  /** Fetch GET /config and rebuild the card state from the llm echo. */
  async function load() {
    await cfg.fetch();
    const { cards: next, primaryProvider } = cardsFromLlmConfig(cfg.config.value?.llm);
    cards.value = next;
    primaryName.value = primaryProvider;
  }

  /** Persist the current card state, then re-read the masked echo. */
  async function persist(successMessage: string): Promise<boolean> {
    let ok = true;
    try {
      await cfg.save(buildLlmPayload(cards.value, primaryName.value));
    } catch {
      ok = false;
      ElMessage({ type: 'error', message: t('config.providerCards.saveFailed') });
    }
    // Re-read either way: on success a typed key normalizes to the `***`
    // mask; on failure the card state resyncs with the server truth.
    await load();
    if (ok) {
      ElMessage({ type: 'success', message: successMessage });
      options.afterSave?.();
    }
    return ok;
  }

  /** Add a new provider card and persist immediately. The first card added
   *  becomes the primary. */
  async function addCard(form: ProviderCardState): Promise<boolean> {
    cards.value = [...cards.value, { ...form }];
    if (!primaryName.value) primaryName.value = form.provider;
    return persist(t('config.providerCards.saved'));
  }

  /** Replace a card's fields with the edited form and persist immediately.
   *  A blank key in the edit dialog means "keep the saved key": the echoed
   *  masked sentinel is restored so only a typed key is ever sent live. */
  async function editCard(originalName: string, form: ProviderCardState): Promise<boolean> {
    const idx = cards.value.findIndex((c) => c.provider === originalName);
    if (idx === -1) return false;
    const keepKey = cards.value[idx].apiKey;
    const next = cards.value.slice();
    next[idx] = { ...form, provider: originalName, apiKey: form.apiKey || keepKey };
    cards.value = next;
    return persist(t('config.providerCards.saved'));
  }

  /** Promote a card to primary and persist immediately. */
  async function setPrimary(card: ProviderCardState): Promise<boolean> {
    if (primaryName.value === card.provider) return true;
    primaryName.value = card.provider;
    return persist(
      t('config.providerCards.primarySet', { name: providerDisplayName(card.provider) }),
    );
  }

  /** Confirm, then delete a card and persist immediately. Deleting the
   *  primary auto-promotes the first remaining provider — the confirm text
   *  says so. */
  async function deleteCard(card: ProviderCardState): Promise<void> {
    const idx = cards.value.findIndex((c) => c.provider === card.provider);
    if (idx === -1) return;
    const name = providerDisplayName(card.provider);
    const remaining = cards.value.filter((_, i) => i !== idx);
    const isPrimary = primaryName.value === card.provider;
    const nextPrimary = isPrimary ? remaining[0] : undefined;
    try {
      await ElMessageBox.confirm(
        nextPrimary
          ? t('config.providerCards.deletePrimaryConfirm', {
              name,
              next: providerDisplayName(nextPrimary.provider),
            })
          : t('config.providerCards.deleteConfirm', { name }),
        t('config.providerCards.deleteTitle'),
        {
          confirmButtonText: t('common.remove'),
          cancelButtonText: t('common.cancel'),
          type: 'warning',
        },
      );
    } catch {
      return; // cancelled
    }

    // Removing the LAST provider cannot be expressed by PUT /config alone:
    // the backend only replaces the runtime provider set when the resolved
    // list is non-empty, and a blank scalar key means "keep". Clear the
    // runtime entry via the CRUD endpoint FIRST (its absence then lets the
    // masked-keep resolution come up empty), then persist the empty
    // projection.
    const isLast = remaining.length === 0;
    if (isLast) {
      const runtime = options.statusProviders.value.find((p) => p.name === card.provider);
      if (runtime) {
        try {
          await deleteProviderApi(runtime.id);
        } catch {
          // Already gone or unreachable — the PUT below still records the
          // intent and the next load resyncs whatever the server reports.
        }
      }
    }
    cards.value = remaining;
    if (isPrimary) primaryName.value = nextPrimary?.provider ?? '';
    await persist(t('config.providerCards.deleted', { name }));
  }

  return {
    cards,
    primaryName,
    loading,
    saving,
    error,
    load,
    addCard,
    editCard,
    setPrimary,
    deleteCard,
  };
}
