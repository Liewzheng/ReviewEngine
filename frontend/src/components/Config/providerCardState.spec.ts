import { describe, expect, it } from 'vitest';
import type { ProviderCardState } from '../../composables/llmPayload';
import {
  cardAccent,
  dialogForDuplicate,
  dialogForEdit,
  cardFooter,
  cardVisualState,
  formatLatency,
  formatRequests,
  formatSuccessRate,
  formatUsagePercent,
  latencyLabelKey,
  latencyReading,
  matchHealthToCards,
  matchProbeMessages,
  statsRow,
  type CardHealth,
} from './providerCardState';

/**
 * Card view-model contract (RENG-75).
 *
 * These are the pieces the grid renders from, and the ones a redesigned card
 * gets wrong silently: which visual state a card is in, what the reserved
 * footer slot says, how an unmeasured value is written down, and — the
 * RENG-75 trap — how a card finds ITS runtime entry now that the provider
 * name is a display label two cards may share.
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

function health(name: string, over: Partial<CardHealth> = {}): CardHealth {
  return {
    name,
    status: 'healthy',
    disabled: false,
    apiBaseUrl: `https://${name}.example/v1`,
    defaultModel: 'model-a',
    ...over,
  };
}

describe('cardVisualState', () => {
  it('reads the config echo’s disabled flag as the disabled state', () => {
    expect(cardVisualState(card('openai', { disabled: true }), health('openai'))).toBe('disabled');
  });

  it('reads the health payload’s disabled status as the disabled state', () => {
    const entry = health('openai', { status: 'disabled', disabled: true });
    expect(cardVisualState(card('openai'), entry)).toBe('disabled');
  });

  it('keeps the health status otherwise, and falls back to offline without one', () => {
    expect(cardVisualState(card('openai'), health('openai', { status: 'error' }))).toBe('error');
    expect(cardVisualState(card('openai'), health('openai', { status: 'degraded' }))).toBe('degraded');
    expect(cardVisualState(card('openai'))).toBe('offline');
  });
});

describe('cardAccent', () => {
  it('marks only error and degraded cards with a stripe', () => {
    expect(cardAccent(card('a'), health('a', { status: 'error' }))).toBe('error');
    expect(cardAccent(card('a'), health('a', { status: 'degraded' }))).toBe('degraded');
    expect(cardAccent(card('a'), health('a'))).toBe('none');
    expect(cardAccent(card('a'), health('a', { status: 'offline' }))).toBe('none');
  });

  it('gives a disabled card no coloured decoration', () => {
    // A stopped card is not a failure: it must not wear the error stripe of
    // the card it became unreachable as.
    expect(cardAccent(card('a', { disabled: true }), health('a', { status: 'error' }))).toBe('none');
  });
});

describe('stats row', () => {
  it('renders the three values in order with label-only tooltips', () => {
    const stats = statsRow(health('a', { avgLatencyMs: 300, requestCount: 7, successRate: 1 }));
    expect(stats.map((s) => s.text)).toEqual(['300ms', '7', '100.0%']);
    expect(stats.map((s) => s.labelKey)).toEqual([
      'llm.metrics.avgLatency',
      'llm.metrics.requests',
      'llm.metrics.successRate',
    ]);
    expect(stats.every((s) => !s.empty)).toBe(true);
  });

  it('writes an unmeasured value as an em dash instead of a zero', () => {
    const stats = statsRow(health('a', { avgLatencyMs: null, requestCount: 0, successRate: null }));
    expect(stats.map((s) => s.text)).toEqual(['—', '0', '—']);
    expect(stats.map((s) => s.empty)).toEqual([true, false, true]);
  });

  it('always produces the same three cells — every state is the same height', () => {
    const states: (CardHealth | undefined)[] = [
      health('a', { status: 'healthy', avgLatencyMs: 300, requestCount: 7, successRate: 1 }),
      health('a', { status: 'degraded', avgLatencyMs: 1800, requestCount: 42, successRate: 0.964 }),
      health('a', { status: 'error', avgLatencyMs: null, requestCount: 128, successRate: 0.821 }),
      health('a', { status: 'disabled', disabled: true }),
      health('a', { status: 'offline' }),
      undefined,
    ];
    for (const entry of states) {
      expect(statsRow(entry)).toHaveLength(3);
    }
  });
});

describe('value formatting', () => {
  it('renders a missing value as an em dash', () => {
    expect(formatLatency(null)).toBe('—');
    expect(formatLatency(undefined)).toBe('—');
    expect(formatRequests(null)).toBe('—');
    expect(formatSuccessRate(null)).toBe('—');
    expect(formatUsagePercent(null)).toBeNull();
  });

  it('formats the values it does have', () => {
    expect(formatLatency(300)).toBe('300ms');
    expect(formatRequests(1234)).toBe('1,234');
    expect(formatSuccessRate(0.964)).toBe('96.4%');
    expect(formatSuccessRate(1)).toBe('100.0%');
    expect(formatUsagePercent(0.253)).toBe(25.3);
    expect(formatUsagePercent(0.0004)).toBe(0);
  });
});

describe('RENG-77 — a duration is one token', () => {
  it('scales milliseconds to seconds to minutes', () => {
    expect(formatLatency(128)).toBe('128ms');
    expect(formatLatency(999)).toBe('999ms');
    expect(formatLatency(1000)).toBe('1.0s');
    expect(formatLatency(1800)).toBe('1.8s');
    expect(formatLatency(17696)).toBe('17.7s');
    expect(formatLatency(60000)).toBe('1.0min');
    expect(formatLatency(123456)).toBe('2.1min');
  });

  it('never introduces a space, so the KPI and the stat cell stay on one line', () => {
    for (const ms of [0, 7, 300, 999, 1000, 1800, 17696, 59999, 60000, 3600000]) {
      expect(formatLatency(ms)).not.toMatch(/\s/);
    }
  });
});

describe('RENG-77 — the latency reading', () => {
  it('prefers the measured communication latency', () => {
    expect(latencyReading(health('a', { avgTtfbMs: 17690, avgLatencyMs: 21000 }))).toEqual({
      ms: 17690,
      source: 'ttfb',
    });
  });

  it('falls back to the recorded call latency when the TTFB is null', () => {
    expect(latencyReading(health('a', { avgTtfbMs: null, avgLatencyMs: 300 }))).toEqual({
      ms: 300,
      source: 'latency',
    });
  });

  it('falls back when the field is absent altogether — an older payload still reads', () => {
    expect(latencyReading(health('a', { avgLatencyMs: 300 }))).toEqual({ ms: 300, source: 'latency' });
  });

  it('reports nothing measured as an empty reading, never a zero', () => {
    expect(latencyReading(health('a', { avgTtfbMs: null, avgLatencyMs: null }))).toEqual({
      ms: null,
      source: null,
    });
    expect(latencyReading(undefined)).toEqual({ ms: null, source: null });
  });

  it('names the measurement so the card labels what it shows', () => {
    expect(latencyLabelKey('ttfb')).toBe('llm.metrics.avgTtfb');
    expect(latencyLabelKey('latency')).toBe('llm.metrics.avgLatency');
    expect(latencyLabelKey(null)).toBe('llm.metrics.avgLatency');
  });

  it('puts the unrounded measurement in the stat tooltip', () => {
    const [latency] = statsRow(health('a', { avgTtfbMs: 17690, avgLatencyMs: 21000 }));
    expect(latency).toEqual({
      key: 'avgLatency',
      labelKey: 'llm.metrics.avgTtfb',
      text: '17.7s',
      empty: false,
      exact: '17690 ms',
    });
    // The fallback keeps the old label and still carries its exact value.
    const [fallback] = statsRow(health('a', { avgTtfbMs: null, avgLatencyMs: 300 }));
    expect(fallback.labelKey).toBe('llm.metrics.avgLatency');
    expect(fallback.exact).toBe('300 ms');
    expect(fallback.text).toBe('300ms');
  });
});

describe('footer slot', () => {
  it('is empty for a healthy enabled card', () => {
    expect(cardFooter(card('a'), health('a'), null)).toEqual({ kind: 'none', detail: '' });
  });

  it('says the card is switched off when it is disabled', () => {
    expect(cardFooter(card('a', { disabled: true }), undefined, null).kind).toBe('disabled');
  });

  it('carries the probe’s failure text for an errored card', () => {
    const footer = cardFooter(
      card('a'),
      health('a', { status: 'error' }),
      'HTTP 401 Unauthorized',
    );
    expect(footer).toEqual({ kind: 'error', detail: 'HTTP 401 Unauthorized' });
  });

  it('reports an errored card with no message without inventing one', () => {
    expect(cardFooter(card('a'), health('a', { status: 'error' }), null).detail).toBe('');
  });

  it('reports offline separately from disabled', () => {
    expect(cardFooter(card('a'), health('a', { status: 'offline' }), null).kind).toBe('offline');
  });

  it('shows the manual test outcome while it is in session state', () => {
    expect(cardFooter(card('a'), health('a'), null, { success: true }).kind).toBe('testOk');
    expect(cardFooter(card('a'), health('a'), null, { success: false, error: 'boom' })).toEqual({
      kind: 'testFail',
      detail: 'boom',
    });
  });
});

describe('matchHealthToCards', () => {
  it('gives each card its own entry when two cards share a provider name', () => {
    // The RENG-75 regression: the name is a label, the triple is the card.
    // Joining on the name showed the first account's latency on both.
    const cards = [
      card('openai', { apiBaseUrl: 'https://a.example/v1', defaultModel: 'gpt-4o' }),
      card('openai', { apiBaseUrl: 'https://b.example/v1', defaultModel: 'gpt-4o' }),
    ];
    const providers = [
      health('openai', { apiBaseUrl: 'https://a.example/v1', avgLatencyMs: 100 }),
      health('openai', { apiBaseUrl: 'https://b.example/v1', avgLatencyMs: 900 }),
    ];
    const matched = matchHealthToCards(cards, providers);
    expect(matched[0]?.avgLatencyMs).toBe(100);
    expect(matched[1]?.avgLatencyMs).toBe(900);
  });

  it('leaves a card without a runtime entry unmatched', () => {
    const cards = [card('openai'), card('ghost')];
    const providers = [health('openai')];
    expect(matchHealthToCards(cards, providers)[0]).toBe(providers[0]);
    expect(matchHealthToCards(cards, providers)[1]).toBeUndefined();
  });

  it('falls back to the stored position when the payload carries no base/model', () => {
    const cards = [card('openai'), card('deepseek')];
    const providers = [
      { ...health('openai'), apiBaseUrl: undefined, defaultModel: undefined },
      { ...health('deepseek'), apiBaseUrl: undefined, defaultModel: undefined },
    ];
    const matched = matchHealthToCards(cards, providers);
    expect(matched[0]?.name).toBe('openai');
    expect(matched[1]?.name).toBe('deepseek');
  });
});

describe('matchProbeMessages', () => {
  it('labels each probe row by provider and model', () => {
    const cards = [card('anthropic', { defaultModel: 'claude-sonnet-4-5' })];
    const rows = [{ service: 'anthropic claude-sonnet-4-5', message: 'HTTP 401 Unauthorized' }];
    expect(matchProbeMessages(cards, rows)).toEqual(['HTTP 401 Unauthorized']);
  });

  it('reports no message for a card the health endpoint did not describe', () => {
    expect(matchProbeMessages([card('anthropic')], [])).toEqual([null]);
  });
});

describe('context-menu dialog contract', () => {
  const cards = [card('xiaomi'), card('deepseek', { apiKey: '***', defaultModel: 'deepseek-v4-flash' })];

  it('edit re-opens the card over its own grid position', () => {
    const open = dialogForEdit(cards, 1);
    expect(open).toEqual({ mode: 'edit', initial: cards[1], index: 1 });
    // A copy, so a cancelled edit cannot have mutated the grid.
    expect(open?.initial).not.toBe(cards[1]);
  });

  it('duplicate opens the ADD form, pre-filled with the card and its key', () => {
    const open = dialogForDuplicate(cards, 1);
    expect(open?.mode).toBe('add');
    expect(open?.initial).toEqual(cards[1]);
    expect(open?.initial.apiKey).toBe('***');
    // -1 = the save appends; the original card is left where it is.
    expect(open?.index).toBe(-1);
  });

  it('reports nothing to open for an index that is not there', () => {
    expect(dialogForEdit(cards, 9)).toBeNull();
    expect(dialogForDuplicate(cards, -1)).toBeNull();
  });
});
