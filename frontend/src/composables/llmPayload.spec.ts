import { describe, expect, it } from 'vitest';
import { buildLlmPayload, type ProviderCardState } from './llmPayload';

/**
 * Card-state → PUT /config payload contract (RENG-72).
 *
 * The regression this pins: an ordinary card add/edit used to re-assert the
 * primary provider it happened to hold, so a save made from a view that
 * predates a primary change (a second tab, a page opened earlier) dragged the
 * stored primary back to the stale value. Only a save that carries the user's
 * primary choice may send `primaryProvider`.
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
    ...over,
  };
}

describe('buildLlmPayload', () => {
  it('asserts the primary and its scalar mirror on a primary-intent save', () => {
    const { llm } = buildLlmPayload([card('xiaomi'), card('deepseek')], 'deepseek', {
      assertPrimary: true,
    });

    expect(llm.primaryProvider).toBe('deepseek');
    expect(llm.defaultModel).toBe('model-a');
    expect(llm.apiBaseUrl).toBe('https://deepseek.example/v1');
    expect(llm.openaiApiKey).toBe('***');
    expect(llm.providers?.map((p) => p.provider)).toEqual(['xiaomi', 'deepseek']);
  });

  it('omits the primary (and the non-openai scalar mirror) on an ordinary save', () => {
    const { llm } = buildLlmPayload([card('xiaomi'), card('deepseek')], 'deepseek', {
      assertPrimary: false,
    });

    // The stale primary must not travel: an absent key keeps the stored value.
    expect('primaryProvider' in llm).toBe(false);
    expect('defaultModel' in llm).toBe(false);
    expect('apiBaseUrl' in llm).toBe(false);
    expect('openaiApiKey' in llm).toBe(false);
    // …while the card the user actually edited is still applied.
    expect(llm.providers).toHaveLength(2);
  });

  it('omits the primary but keeps the scalars when the primary IS openai', () => {
    // The legacy scalar fields are the only path the backend applies an
    // `openai` primary through, so dropping them would drop the edit.
    const { llm } = buildLlmPayload(
      [card('openai', { apiKey: 'sk-live', defaultModel: 'gpt-4o' }), card('deepseek')],
      'openai',
      { assertPrimary: false },
    );

    expect('primaryProvider' in llm).toBe(false);
    expect(llm.openaiApiKey).toBe('sk-live');
    expect(llm.defaultModel).toBe('gpt-4o');
  });

  it('defaults to asserting the primary when no options are given', () => {
    const { llm } = buildLlmPayload([card('deepseek')], 'deepseek');
    expect(llm.primaryProvider).toBe('deepseek');
  });

  it('persists an explicit empty projection when every provider is gone', () => {
    const { llm } = buildLlmPayload([], '', { assertPrimary: true });
    expect(llm.primaryProvider).toBe('');
    expect(llm.providers).toEqual([]);
  });
});
