import { describe, expect, it } from 'vitest';
import {
  buildLlmPayload,
  cardKey,
  cardsFromLlmConfig,
  chainHeadName,
  duplicateCard,
  reorderCards,
  sameCard,
  type ProviderCardState,
} from './llmPayload';
import type { LLMConfig } from '../types/config';

/**
 * Drag-to-reorder contract (RENG-75).
 *
 * Dropping a card persists the new order through `PUT /config`, and since
 * RENG-75 the stored order IS the priority rule: the first ENABLED card of
 * the new order has to become the chain head the runtime walks, or the grid
 * would show an order the chain does not follow.
 */

function card(provider: string, over: Partial<ProviderCardState> = {}): ProviderCardState {
  return {
    provider,
    apiKey: '***',
    apiBaseUrl: `https://${provider}.example/v1`,
    defaultModel: 'model-a',
    maxTokens: 4096,
    temperature: 0.7,
    timeoutSeconds: 60,
    retryAttempts: 3,
    disabled: false,
    ...over,
  };
}

/** A GET /config echo carrying the given provider list. */
function llmEcho(cards: ProviderCardState[], primaryProvider: string): LLMConfig {
  return {
    apiBaseUrl: cards[0]?.apiBaseUrl ?? '',
    openaiApiKey: '***',
    defaultModel: cards[0]?.defaultModel ?? '',
    maxTokens: 4096,
    temperature: 0.7,
    timeoutSeconds: 60,
    retryAttempts: 3,
    primaryProvider,
    providers: cards.map((c) => ({
      provider: c.provider,
      apiKey: c.apiKey,
      apiBaseUrl: c.apiBaseUrl,
      defaultModel: c.defaultModel,
      maxTokens: c.maxTokens,
      temperature: c.temperature,
      timeoutSeconds: c.timeoutSeconds,
      retryAttempts: c.retryAttempts,
      disabled: c.disabled,
    })),
  };
}

describe('reorderCards', () => {
  it('moves a card forward and backward without touching the others', () => {
    const cards = [card('a'), card('b'), card('c')];
    expect(reorderCards(cards, 0, 2).map((c) => c.provider)).toEqual(['b', 'c', 'a']);
    expect(reorderCards(cards, 2, 0).map((c) => c.provider)).toEqual(['c', 'a', 'b']);
    expect(cards.map((c) => c.provider)).toEqual(['a', 'b', 'c']);
  });

  it('ignores an out-of-range drop instead of corrupting the list', () => {
    const cards = [card('a'), card('b')];
    expect(reorderCards(cards, 0, 9)).toEqual(cards);
    expect(reorderCards(cards, -1, 0)).toEqual(cards);
  });
});

describe('chainHeadName', () => {
  it('is the first enabled card', () => {
    expect(chainHeadName([card('a'), card('b')])).toBe('a');
    expect(chainHeadName([card('a', { disabled: true }), card('b')])).toBe('b');
  });

  it('is empty when every card is switched off', () => {
    expect(chainHeadName([card('a', { disabled: true })])).toBe('');
    expect(chainHeadName([])).toBe('');
  });
});

describe('a reorder persists the new order and its chain head', () => {
  const stored = [card('xiaomi'), card('deepseek'), card('anthropic')];

  it('sends providers[] in the dropped order', () => {
    const next = reorderCards(stored, 2, 0);
    const head = chainHeadName(next);
    const { llm } = buildLlmPayload(next, head, { assertPrimary: head !== '' });

    expect(llm.providers?.map((p) => p.provider)).toEqual(['anthropic', 'xiaomi', 'deepseek']);
    expect(llm.primaryProvider).toBe('anthropic');
  });

  it('skips a disabled card when naming the new head', () => {
    const cards = [card('xiaomi', { disabled: true }), card('deepseek')];
    const next = reorderCards(cards, 1, 0);
    const head = chainHeadName(next);
    const { llm } = buildLlmPayload(next, head, { assertPrimary: head !== '' });

    expect(llm.providers?.map((p) => p.provider)).toEqual(['deepseek', 'xiaomi']);
    // The disabled card keeps its own flag; the head is the enabled one.
    expect(llm.providers?.map((p) => p.disabled)).toEqual([false, true]);
    expect(llm.primaryProvider).toBe('deepseek');
  });

  it('leaves the stored primary alone when no card is enabled', () => {
    const cards = [card('xiaomi', { disabled: true }), card('deepseek', { disabled: true })];
    const head = chainHeadName(reorderCards(cards, 1, 0));
    const { llm } = buildLlmPayload(reorderCards(cards, 1, 0), head, { assertPrimary: head !== '' });

    expect(head).toBe('');
    expect('primaryProvider' in llm).toBe(false);
  });
});

describe('the disabled flag round-trips', () => {
  it('is read from the config echo', () => {
    const { cards } = cardsFromLlmConfig(
      llmEcho([card('xiaomi'), card('deepseek', { disabled: true })], 'xiaomi'),
    );
    expect(cards.map((c) => c.disabled)).toEqual([false, true]);
  });

  it('defaults to enabled when the echo predates the field', () => {
    const echo = llmEcho([card('xiaomi')], 'xiaomi');
    delete echo.providers[0].disabled;
    expect(cardsFromLlmConfig(echo).cards[0].disabled).toBe(false);
  });

  it('is written back explicitly on every save', () => {
    const { llm } = buildLlmPayload([card('xiaomi'), card('deepseek', { disabled: true })], 'xiaomi', {
      assertPrimary: false,
    });
    expect(llm.providers?.[1].disabled).toBe(true);
    expect(llm.providers?.[0].disabled).toBe(false);
  });
});

describe('card identity', () => {
  it('follows the triple, so two same-named cards are different cards', () => {
    const a = card('openai', { apiBaseUrl: 'https://a.example/v1' });
    const b = card('openai', { apiBaseUrl: 'https://b.example/v1' });
    const sameNameSameTriple = card('openai', { apiBaseUrl: 'https://a.example/v1' });

    expect(cardKey(a)).not.toBe(cardKey(b));
    expect(sameCard(a, b)).toBe(false);
    expect(sameCard(a, sameNameSameTriple)).toBe(true);
  });
});

describe('duplicateCard', () => {
  it('starts from the whole card, masked key included', () => {
    const original = card('xiaomi', { apiKey: '***', defaultModel: 'mimo-v2.5' });
    const copy = duplicateCard(original);

    expect(copy).toEqual(original);
    expect(copy).not.toBe(original);
    // The copy is a sibling of the original, not a replacement for it.
    copy.defaultModel = 'mimo-v2.6';
    expect(original.defaultModel).toBe('mimo-v2.5');
  });
});
