import { describe, expect, it } from 'vitest';
import type { ProviderCardState } from '../../composables/llmPayload';
import en from '../../i18n/locales/en';
import zhCN from '../../i18n/locales/zh-CN';
import zhTW from '../../i18n/locales/zh-TW';
import ja from '../../i18n/locales/ja';
import ko from '../../i18n/locales/ko';
import fr from '../../i18n/locales/fr';
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
  stripLatency,
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

describe('RENG-78 — the latency reading', () => {
  it('prefers the probe average: the one number with no model in it', () => {
    expect(
      latencyReading(health('a', { avgProbeLatencyMs: 240, avgLatencyMs: 21000, avgTtfbMs: 20980 })),
    ).toEqual({ ms: 240, source: 'probe' });
  });

  it('falls back to the recorded call latency when no probe average exists', () => {
    expect(latencyReading(health('a', { avgProbeLatencyMs: null, avgLatencyMs: 300 }))).toEqual({
      ms: 300,
      source: 'latency',
    });
  });

  it('falls back when the field is absent altogether — an older payload still reads', () => {
    expect(latencyReading(health('a', { avgLatencyMs: 300 }))).toEqual({ ms: 300, source: 'latency' });
  });

  it('no longer shows the TTFB: for a non-streaming provider it is ≈ the call latency', () => {
    // `avgTtfbMs` is still on the payload (RENG-77) but is not a network
    // metric, so the card falls past it to the recorded call latency.
    expect(latencyReading(health('a', { avgTtfbMs: 17690, avgLatencyMs: 21000 }))).toEqual({
      ms: 21000,
      source: 'latency',
    });
  });

  it('reports nothing measured as an empty reading, never a zero', () => {
    expect(
      latencyReading(health('a', { avgProbeLatencyMs: null, avgLatencyMs: null, avgTtfbMs: null })),
    ).toEqual({ ms: null, source: null });
    expect(latencyReading(undefined)).toEqual({ ms: null, source: null });
  });

  it('names the measurement so the card labels what it shows', () => {
    expect(latencyLabelKey('probe')).toBe('llm.metrics.avgCommLatency');
    expect(latencyLabelKey('latency')).toBe('llm.metrics.avgLatency');
    expect(latencyLabelKey(null)).toBe('llm.metrics.avgLatency');
  });

  it('puts the unrounded measurement in the stat tooltip', () => {
    const [latency] = statsRow(health('a', { avgProbeLatencyMs: 240, avgLatencyMs: 21000 }));
    expect(latency).toEqual({
      key: 'avgLatency',
      labelKey: 'llm.metrics.avgCommLatency',
      text: '240ms',
      empty: false,
      exact: '240 ms',
    });
    // The fallback keeps the old label and still carries its exact value.
    const [fallback] = statsRow(health('a', { avgProbeLatencyMs: null, avgLatencyMs: 300 }));
    expect(fallback.labelKey).toBe('llm.metrics.avgLatency');
    expect(fallback.exact).toBe('300 ms');
    expect(fallback.text).toBe('300ms');
  });
});

/**
 * RENG-78: the tooltip names the measurement — 平均通信延迟 / "Avg comm.
 * latency" — and never the mechanism behind it. The probe is an
 * implementation detail; the user is reading a duration.
 */
describe('RENG-78 — the communication-latency label in every locale', () => {
  const locales = { en, 'zh-CN': zhCN, 'zh-TW': zhTW, ja, ko, fr };

  /** Resolve a dotted key path inside a locale object. */
  function message(locale: object, path: string): string {
    return path.split('.').reduce<unknown>((node, part) => (node as Record<string, unknown>)?.[part], locale) as string;
  }

  it('carries the label behind the reading, in all six locales', () => {
    const key = latencyLabelKey('probe');
    expect(key).toBe('llm.metrics.avgCommLatency');
    for (const [name, messages] of Object.entries(locales)) {
      expect(message(messages, key), `${name} is missing ${key}`).toBeTruthy();
      expect(message(messages, 'llm.metrics.avgCommLatency')).toBe(message(messages, 'llm.metrics.avgTtfb'));
    }
    // The Chinese locales spell it the way the requirement does.
    expect(message(zhCN, key)).toBe('平均通信延迟');
    expect(message(zhTW, key)).toBe('平均通訊延遲');
  });

  it('never says the number came from a probe', () => {
    const key = latencyLabelKey('probe');
    for (const [name, messages] of Object.entries(locales)) {
      const label = message(messages, key);
      expect(label, `${name}: ${label}`).not.toContain('探测');
      expect(label, `${name}: ${label}`).not.toContain('探測');
      expect(label.toLowerCase(), `${name}: ${label}`).not.toContain('probe');
    }
  });

  it('names the strip’s number with the card’s wording', () => {
    // The two blocks differ in letter case only (en/fr sentence case vs title
    // case, as the existing `avgTtfb` pair does): the words are the same, so
    // the KPI strip and the card can never look like two measurements.
    for (const [name, messages] of Object.entries(locales)) {
      const strip = message(messages, 'llm.stats.avgCommLatency');
      expect(strip, `${name} is missing the strip label`).toBeTruthy();
      expect(strip.toLowerCase(), `${name}: ${strip}`).toBe(
        message(messages, 'llm.metrics.avgCommLatency').toLowerCase(),
      );
    }
    expect(message(zhCN, 'llm.stats.avgCommLatency')).toBe('平均通信延迟');
    expect(message(zhTW, 'llm.stats.avgCommLatency')).toBe('平均通訊延遲');
  });
});

describe('RENG-78 — the KPI strip’s number', () => {
  it('weights each provider by the samples behind ITS reading', () => {
    const strip = stripLatency([
      // A probe reading with 4 samples: 40 ms.
      health('a', { avgProbeLatencyMs: 40, probeSampleCount: 4, latencySampleCount: 100 }),
      // A fallback call reading with 1 sample: 1000 ms.
      health('b', { avgProbeLatencyMs: null, avgLatencyMs: 1000, latencySampleCount: 1 }),
    ]);
    // (40 * 4 + 1000 * 1) / 5 = 232 — NOT weighted by the call counts (which
    // would be 45), and not the average of the two averages (520).
    expect(strip).toEqual({ ms: 232, source: 'probe' });
  });

  it('names the number communication latency as soon as one reading is a probe’s', () => {
    const withOneProbe = stripLatency([
      health('a', { avgProbeLatencyMs: 40, probeSampleCount: 1 }),
      health('b', { avgProbeLatencyMs: null, avgLatencyMs: 1000, latencySampleCount: 1 }),
    ]);
    expect(withOneProbe.source).toBe('probe');
    expect(latencyLabelKey(withOneProbe.source)).toBe('llm.metrics.avgCommLatency');

    // Every reading fell back: the strip says so instead of claiming a
    // communication latency nobody measured.
    const allFallback = stripLatency([
      health('b', { avgProbeLatencyMs: null, avgLatencyMs: 1000, latencySampleCount: 1 }),
    ]);
    expect(allFallback).toEqual({ ms: 1000, source: 'latency' });
    expect(latencyLabelKey(allFallback.source)).toBe('llm.metrics.avgLatency');
  });

  it('is an empty reading when nothing was measured, never a zero', () => {
    expect(stripLatency([])).toEqual({ ms: null, source: null });
    // A reading with no sample behind it carries no weight: it is not evidence.
    expect(stripLatency([health('a', { avgProbeLatencyMs: 40, probeSampleCount: 0 })])).toEqual({
      ms: null,
      source: null,
    });
    expect(
      stripLatency([health('a', { avgProbeLatencyMs: null, avgLatencyMs: null, latencySampleCount: 3 })]),
    ).toEqual({ ms: null, source: null });
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
