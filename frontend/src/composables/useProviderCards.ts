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
 * Only a save that carries the user's primary choice (set-as-primary, the
 * first card of an empty page, dropping the last provider) sends
 * `primaryProvider`; add/edit leave it out so the stored primary survives a
 * save made from a view that predates it (RENG-72). Deleting the primary is
 * refused outright — the successor is the user's to name.
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

  /** Persist the current card state, then re-read the masked echo.
   *  `opts.assertPrimary` marks a save that carries the user's primary
   *  choice — every other save leaves the stored primary alone (see
   *  {@link buildLlmPayload}). */
  async function persist(
    successMessage: string,
    opts: { assertPrimary?: boolean } = {},
  ): Promise<boolean> {
    let ok = true;
    try {
      await cfg.save(buildLlmPayload(cards.value, primaryName.value, opts));
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
   *  becomes the primary — that is an explicit choice, so this save carries
   *  it; adding a card alongside existing ones never does. */
  async function addCard(form: ProviderCardState): Promise<boolean> {
    const becomesPrimary = !primaryName.value;
    cards.value = [...cards.value, { ...form }];
    if (becomesPrimary) primaryName.value = form.provider;
    return persist(t('config.providerCards.saved'), { assertPrimary: becomesPrimary });
  }

  /** Replace a card's fields with the edited form and persist immediately.
   *  A blank key in the edit dialog means "keep the saved key": the echoed
   *  masked sentinel is restored so only a typed key is ever sent live.
   *  An edit never speaks for the primary: the payload omits it, so a stale
   *  view cannot drag the stored primary back to this card. */
  async function editCard(originalName: string, form: ProviderCardState): Promise<boolean> {
    const idx = cards.value.findIndex((c) => c.provider === originalName);
    if (idx === -1) return false;
    const keepKey = cards.value[idx].apiKey;
    const next = cards.value.slice();
    next[idx] = { ...form, provider: originalName, apiKey: form.apiKey || keepKey };
    cards.value = next;
    return persist(t('config.providerCards.saved'), { assertPrimary: false });
  }

  /** Promote a card to primary and persist immediately. */
  async function setPrimary(card: ProviderCardState): Promise<boolean> {
    if (primaryName.value === card.provider) return true;
    primaryName.value = card.provider;
    return persist(
      t('config.providerCards.primarySet', { name: providerDisplayName(card.provider) }),
      { assertPrimary: true },
    );
  }

  /** Confirm, then delete a card and persist immediately.
   *
   *  Deleting the PRIMARY is refused while other providers remain: the
   *  successor would be the array head, a provider the user never chose, and
   *  that implicit promotion is what silently moved the primary off a
   *  provider the user had set (RENG-72, "clear the primary card, re-add it"
   *  ended with the head as primary). The user designates the successor
   *  explicitly instead — always a `setPrimary` save, never a side effect of
   *  a deletion. Deleting the LAST card is still allowed: it clears the
   *  primary and the runtime has no provider left. */
  async function deleteCard(card: ProviderCardState): Promise<void> {
    const idx = cards.value.findIndex((c) => c.provider === card.provider);
    if (idx === -1) return;
    const name = providerDisplayName(card.provider);
    const remaining = cards.value.filter((_, i) => i !== idx);
    const isPrimary = primaryName.value === card.provider;

    if (isPrimary && remaining.length > 0) {
      try {
        await ElMessageBox.alert(
          t('config.providerCards.deletePrimaryRequiresSelection', { name }),
          t('config.providerCards.deleteTitle'),
          { confirmButtonText: t('common.ok'), type: 'warning' },
        );
      } catch {
        // Dismissed via Esc/close — nothing was changed either way.
      }
      return;
    }

    try {
      await ElMessageBox.confirm(
        t('config.providerCards.deleteConfirm', { name }),
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
    // projection — which is what clears the stored primary.
    if (remaining.length === 0) {
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
    if (isPrimary) primaryName.value = '';
    // Only the deletion of the primary speaks for the primary: a non-primary
    // deletion leaves it to the stored value, so it cannot drag back a stale
    // one either. The last-card case is `isPrimary` by construction and
    // persists the explicit empty projection.
    await persist(t('config.providerCards.deleted', { name }), { assertPrimary: isPrimary });
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
