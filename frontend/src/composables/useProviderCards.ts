import { ref, type Ref } from 'vue';
import { ElMessage, ElMessageBox } from 'element-plus';
import { useI18n } from 'vue-i18n';
import { useConfig } from './useConfig';
import { deleteProvider as deleteProviderApi } from '../services/llm';
import {
  buildLlmPayload,
  cardsFromLlmConfig,
  chainHeadName,
  providerDisplayName,
  reorderCards as reorderPure,
  type ProviderCardState,
} from './llmPayload';
import type { LlmProvider } from '../types/llm';

/**
 * Unified provider-card model for the /llm page configuration section.
 *
 * One card per configured provider (the primary included), driven by the
 * GET /config `llm` echo. Every add/edit/delete/toggle/reorder mutation is
 * persisted immediately as a sparse `PUT {llm}` (hot-applied + written
 * through) with ElMessage feedback — there is no page-level edit mode.
 *
 * A card is addressed by its INDEX in the grid, never by its provider name:
 * RENG-75 made the name a free-form display label, so two cards may carry
 * the same one (two accounts, or one account × two models) and a name lookup
 * would edit the wrong entry.
 *
 * Only a save that carries the user's primary choice (set-as-primary, the
 * first card of an empty page, a reorder that moves the chain head, dropping
 * the last provider) sends `primaryProvider`; add/edit leave it out so the
 * stored primary survives a save made from a view that predates it (RENG-72).
 * Since RENG-75 the stored ORDER is the priority rule, so a reorder restates
 * the head as the first ENABLED card of the new order. Deleting the head
 * while an enabled provider remains is refused — the successor is the user's
 * to designate by dragging it to the front.
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

  /** Fetch GET /config and rebuild the card state from the llm echo.
   *  A build with `VITE_USE_LLM_MOCKS=true` installs the mock card set
   *  instead — the same shape the echo yields, so nothing downstream knows. */
  async function load() {
    if (import.meta.env.VITE_USE_LLM_MOCKS === 'true') {
      const { MOCK_CARDS, MOCK_PRIMARY } = await import('../dev-mocks/llm-providers.mock');
      cards.value = MOCK_CARDS.map((c) => ({ ...c }));
      primaryName.value = MOCK_PRIMARY;
      return;
    }
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
    // mask; on failure the card state resyncs with the server truth — which
    // is also what reverts an optimistic reorder, since the local order that
    // failed to persist is simply replaced by the stored one.
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
  async function editCard(index: number, form: ProviderCardState): Promise<boolean> {
    const previous = cards.value[index];
    if (!previous) return false;
    const next = cards.value.slice();
    next[index] = { ...form, provider: previous.provider, apiKey: form.apiKey || previous.apiKey };
    cards.value = next;
    return persist(t('config.providerCards.saved'), { assertPrimary: false });
  }

  /** Promote a card to primary and persist immediately. */
  async function setPrimary(index: number): Promise<boolean> {
    const card = cards.value[index];
    if (!card || primaryName.value === card.provider) return true;
    primaryName.value = card.provider;
    return persist(
      t('config.providerCards.primarySet', { name: providerDisplayName(card.provider) }),
      { assertPrimary: true },
    );
  }

  /**
   * Switch a card off (or back on) and persist immediately.
   *
   * The stored primary is left alone: the server ignores a primary that is
   * disabled and falls back to the first ENABLED entry, so the chain head
   * follows the switch without the UI having to name a successor — and
   * re-enabling restores exactly the provider the user had chosen.
   */
  async function toggleDisabled(index: number): Promise<boolean> {
    const card = cards.value[index];
    if (!card) return false;
    const next = cards.value.slice();
    next[index] = { ...card, disabled: !card.disabled };
    cards.value = next;
    return persist(
      card.disabled
        ? t('config.providerCards.enabled', { name: providerDisplayName(card.provider) })
        : t('config.providerCards.disabled', { name: providerDisplayName(card.provider) }),
      { assertPrimary: false },
    );
  }

  /**
   * Move a card to a new position and persist the new order.
   *
   * RENG-75 made the stored order the priority rule, so the chain head is
   * simply the first ENABLED card of the new order: the save restates
   * `primaryProvider` as that card (or leaves it alone when every card is
   * disabled and there is no head to name). The update is optimistic — the
   * grid reorders on drop — and a failed save reverts through the reload in
   * {@link persist}.
   */
  async function reorder(fromIndex: number, toIndex: number): Promise<boolean> {
    const next = reorderPure(cards.value, fromIndex, toIndex);
    if (next.length === cards.value.length && next.every((c, i) => c === cards.value[i])) {
      return true;
    }
    const head = chainHeadName(next);
    cards.value = next;
    primaryName.value = head;
    return persist(t('config.providerCards.reordered'), { assertPrimary: head !== '' });
  }

  /** Confirm, then delete a card and persist immediately.
   *
   *  Deleting the CHAIN HEAD is refused while an enabled provider remains:
   *  the successor would be the next enabled card of the stored order, which
   *  is what the chain would silently move onto (RENG-72). The head is
   *  resolved by POSITION, not by name — RENG-75 made the name a display
   *  label two cards may share, so a name comparison would refuse to delete
   *  the wrong card — and a head with no enabled successor is deletable:
   *  refusing there would leave the user with no move at all. Deleting the
   *  last card is allowed too: it clears the primary and the runtime has no
   *  provider left. */
  async function deleteCard(index: number): Promise<void> {
    const card = cards.value[index];
    if (!card) return;
    const name = providerDisplayName(card.provider);
    const remaining = cards.value.filter((_, i) => i !== index);
    const isPrimary = index === cards.value.findIndex((c) => !c.disabled);
    const enabledSuccessor = remaining.some((c) => !c.disabled);

    if (isPrimary && enabledSuccessor) {
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
    toggleDisabled,
    reorder,
    deleteCard,
  };
}
