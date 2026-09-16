import type { LLMConfig } from '../types/config';
import { PROVIDER_TYPES } from '../types/llm';

/**
 * Pure mapping between the GET /config `llm` section and the unified
 * provider-card model on the /llm page — and back into the sparse
 * `PUT {llm}` payload. Kept free of Vue/Element Plus imports so the
 * round-trip contract can be verified in isolation.
 *
 * The backend contract (unchanged):
 * - The legacy scalar fields (`openaiApiKey`, `apiBaseUrl`, `defaultModel`,
 *   `maxTokens`, `temperature`, `timeoutSeconds`, `retryAttempts`) describe
 *   the PRIMARY provider, whatever its name.
 * - `openaiApiKey` echoes `***` when the primary has a stored key, else `''`;
 *   submitting `***` or `''` keeps the stored secret ("masked keep").
 * - `llm.providers[]` carries EVERY configured provider — the primary
 *   included — with the same masked-keep semantics per entry.
 * - For a non-openai primary a live key must never travel in the scalar
 *   field: the backend's legacy path rebuilds that entry under a hardcoded
 *   `openai` label (the v0.9.34 quirk). A newly typed key for a non-openai
 *   primary rides in its providers[] entry instead; the scalar stays masked.
 */

/** One provider card in the unified /llm config grid. Field names mirror the
 *  `llm.providers[]` echo so a load→save round-trip with zero edits re-emits
 *  exactly what GET /config returned. */
export interface ProviderCardState {
  /** Provider type id (e.g. `openai`, `deepseek`); part of the card's identity. */
  provider: string;
  /** Masked (`***`)/empty echo, or a newly typed key pending its first save. */
  apiKey: string;
  apiBaseUrl: string;
  defaultModel: string;
  maxTokens: number;
  temperature: number;
  timeoutSeconds: number;
  retryAttempts: number;
  /**
   * RENG-75 administrative off switch. Always sent as a concrete bool: the
   * backend keeps the stored flag only when the key is ABSENT, and a card the
   * UI loaded carries the state the user last saw.
   */
  disabled?: boolean;
  /**
   * RENG-77: skip the provider's reasoning phase. A reasoning model otherwise
   * spends its whole output budget on hidden reasoning and returns an empty
   * answer — the P0 UAT symptom on `deepseek-v4-flash`. Same keep-on-omit
   * contract as {@link disabled}, and sent as a concrete bool for the same
   * reason: an untouched switch keeps the value the card was loaded with.
   */
  disableThinking?: boolean;
}

/** A sparse `llm` section of a PUT /config payload: `providers` is always
 *  present (the array replaces the stored set), every other key is optional —
 *  an omitted key keeps the stored value (the backend deep-merges). */
export type LlmConfigPatch = Partial<LLMConfig> & Pick<LLMConfig, 'providers'>;

/** Options for {@link buildLlmPayload}. */
export interface LlmPayloadOptions {
  /**
   * True when this save EXPRESSES the user's primary choice (set-as-primary,
   * the first card of an empty page, or the deletion of the last provider).
   * False for an ordinary add/edit, which then omits `primaryProvider` — and,
   * unless the legacy scalar path is how the primary is applied (`openai`),
   * its scalar mirror too — so the server keeps the stored primary.
   *
   * Without this an unrelated save from a view that predates a primary change
   * (a second tab, a page opened earlier) carried the stale primary back and
   * silently rewrote the user's choice (RENG-72).
   */
  assertPrimary?: boolean;
}

/** Defaults applied to provider numeric fields (mirrors the backend's). */
export const PROVIDER_FIELD_DEFAULTS = {
  maxTokens: 4096,
  temperature: 0.7,
  timeoutSeconds: 60,
  retryAttempts: 3,
} as const;

/** Blank card model for the Add Provider dialog. */
export function createEmptyProviderCard(): ProviderCardState {
  return {
    provider: 'custom',
    apiKey: '',
    apiBaseUrl: '',
    defaultModel: '',
    ...PROVIDER_FIELD_DEFAULTS,
    disabled: false,
    disableThinking: false,
  };
}

/** Display label for a provider id: the preset list's label when known. */
export function providerDisplayName(provider: string): string {
  return PROVIDER_TYPES.find((pt) => pt.value === provider)?.label ?? provider;
}

/** True when the scalar fields describe a configured provider even though
 *  the echo carried no providers[] entries (a projection written by an older
 *  backend). The card grid then synthesizes the primary card from them. */
function scalarsLookConfigured(llm: LLMConfig): boolean {
  return !!(llm.openaiApiKey || llm.defaultModel || llm.primaryProvider);
}

/** Map the GET /config `llm` section onto the card grid state. */
export function cardsFromLlmConfig(llm: LLMConfig | null | undefined): {
  cards: ProviderCardState[];
  primaryProvider: string;
} {
  if (!llm) return { cards: [], primaryProvider: '' };
  const cards: ProviderCardState[] = (llm.providers ?? []).map((p) => ({
    provider: p.provider,
    apiKey: p.apiKey ?? '',
    apiBaseUrl: p.apiBaseUrl ?? '',
    defaultModel: p.defaultModel ?? '',
    maxTokens: p.maxTokens ?? PROVIDER_FIELD_DEFAULTS.maxTokens,
    temperature: p.temperature ?? PROVIDER_FIELD_DEFAULTS.temperature,
    timeoutSeconds: p.timeoutSeconds ?? PROVIDER_FIELD_DEFAULTS.timeoutSeconds,
    retryAttempts: p.retryAttempts ?? PROVIDER_FIELD_DEFAULTS.retryAttempts,
    disabled: p.disabled ?? false,
    disableThinking: p.disableThinking ?? false,
  }));
  if (cards.length === 0 && scalarsLookConfigured(llm)) {
    // Legacy projection without providers[]: reconstruct the primary card
    // from the scalar fields (the backend's own restart replay rebuilds the
    // providers array the same way).
    cards.push({
      provider: llm.primaryProvider || 'openai',
      apiKey: llm.openaiApiKey,
      apiBaseUrl: llm.apiBaseUrl,
      defaultModel: llm.defaultModel,
      maxTokens: llm.maxTokens,
      temperature: llm.temperature,
      timeoutSeconds: llm.timeoutSeconds,
      retryAttempts: llm.retryAttempts,
    });
  }
  return { cards, primaryProvider: llm.primaryProvider ?? '' };
}

/**
 * Assemble the sparse PUT /config payload for the current card state.
 * A load→save round-trip with zero user edits re-emits the GET /config echo
 * field-for-field (masked keys included), so an unchanged config changes
 * nothing server-side.
 *
 * `options.assertPrimary` (default true) decides whether the payload speaks
 * for the primary provider — see {@link LlmPayloadOptions.assertPrimary}.
 * Everything in `providers[]` is sent on every save; only the primary fields
 * are made conditional, so an ordinary add/edit still applies its own card.
 */
export function buildLlmPayload(
  cards: ProviderCardState[],
  primaryProvider: string,
  options: LlmPayloadOptions = {},
): { llm: LlmConfigPatch } {
  const assertPrimary = options.assertPrimary ?? true;
  const providers = cards.map((c) => ({
    provider: c.provider,
    apiKey: c.apiKey,
    apiBaseUrl: c.apiBaseUrl,
    defaultModel: c.defaultModel,
    maxTokens: c.maxTokens,
    temperature: c.temperature,
    timeoutSeconds: c.timeoutSeconds,
    retryAttempts: c.retryAttempts,
    disabled: c.disabled ?? false,
    disableThinking: c.disableThinking ?? false,
  }));
  const primary = cards.find((c) => c.provider === primaryProvider) ?? cards[0];
  if (!primary) {
    // Every provider was deleted: persist an explicit empty projection.
    return {
      llm: {
        primaryProvider: '',
        openaiApiKey: '',
        apiBaseUrl: '',
        defaultModel: '',
        ...PROVIDER_FIELD_DEFAULTS,
        providers: [],
      },
    };
  }
  const patch: LlmConfigPatch = { providers };
  if (assertPrimary) {
    patch.primaryProvider = primary.provider;
  }
  // The scalar fields mirror the PRIMARY provider, so they follow the same
  // rule: only a save that speaks for the primary may rewrite them — except
  // when the primary IS `openai`, where the legacy scalars are the only path
  // the backend applies that provider through, so omitting them would drop
  // the edit the user just made to its card.
  if (assertPrimary || primary.provider === 'openai') {
    // The scalar key echoes the PRIMARY provider's key with masked-keep
    // semantics. A live typed key is only sent here when the primary IS
    // `openai`; for a non-openai primary it would be relabeled `openai` by
    // the backend's legacy scalar path, so it rides in providers[] instead
    // and the scalar stays masked (`***` when the primary has a key).
    patch.openaiApiKey =
      primary.provider === 'openai' ? primary.apiKey : primary.apiKey ? '***' : '';
    patch.apiBaseUrl = primary.apiBaseUrl;
    patch.defaultModel = primary.defaultModel;
    patch.maxTokens = primary.maxTokens;
    patch.temperature = primary.temperature;
    patch.timeoutSeconds = primary.timeoutSeconds;
    patch.retryAttempts = primary.retryAttempts;
  }
  return { llm: patch };
}

/**
 * Identity of a card's CONFIGURATION — the `(provider, apiBaseUrl,
 * defaultModel)` triple the UI can see. RENG-75 made the provider name a
 * free-form display label (two cards may share it: two accounts, or one
 * account × two models), so nothing in the UI may key a card by its name.
 * The server-side identity is the same triple plus the API key, hashed into
 * a fingerprint that never leaves the backend — the key-less triple is the
 * closest the client can get, and it is what `PUT /config`'s masked-keep
 * resolution matches on too.
 */
export function cardKey(card: ProviderCardState): string {
  return [card.provider, card.apiBaseUrl, card.defaultModel].join('\u0000');
}

/** Whether two cards describe the same configuration (see {@link cardKey}). */
export function sameCard(a: ProviderCardState, b: ProviderCardState): boolean {
  return cardKey(a) === cardKey(b);
}

/**
 * Move `fromIndex` to `toIndex`, returning a new array — the order the grid
 * renders and the order `PUT /config` persists (`llm_providers.raw.position`
 * follows the array). Out-of-range indices are a no-op copy, so a stray drop
 * event can never corrupt the list.
 */
export function reorderCards(
  cards: ProviderCardState[],
  fromIndex: number,
  toIndex: number,
): ProviderCardState[] {
  const next = cards.slice();
  if (fromIndex < 0 || fromIndex >= next.length || toIndex < 0 || toIndex >= next.length) {
    return next;
  }
  const [moved] = next.splice(fromIndex, 1);
  next.splice(toIndex, 0, moved);
  return next;
}

/**
 * The chain head a given card order implies: the first ENABLED card, or `''`
 * when every card is disabled (the runtime then has no provider to run on).
 * RENG-75 made stored order the priority rule, so this is the single source
 * of the "primary" marker after a reorder — `isPrimary` in the health payload
 * echoes it back.
 */
export function chainHeadName(cards: ProviderCardState[]): string {
  return cards.find((c) => !c.disabled)?.provider ?? '';
}

/**
 * The card a "duplicate" action starts from: the same fields, `apiKey`
 * included. The echo carries the `***` sentinel rather than the secret (the
 * key never leaves the server), and the masked-keep rule resolves that
 * sentinel to the ORIGINAL entry's stored key when the new card's triple
 * still matches it — so the copy starts life as a real sibling holding the
 * same credential, and only a field edit makes it distinct.
 */
export function duplicateCard(card: ProviderCardState): ProviderCardState {
  return { ...card };
}
