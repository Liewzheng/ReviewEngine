import { beforeEach, describe, expect, it, vi } from 'vitest';
import { createSSRApp, type Ref } from 'vue';
import { renderToString } from '@vue/server-renderer';
import { createI18n } from 'vue-i18n';
import { ID_INJECTION_KEY, ZINDEX_INJECTION_KEY } from 'element-plus';
import LlmStatus from './LlmStatus.vue';
import en from '../i18n/locales/en';
import zhCN from '../i18n/locales/zh-CN';

/**
 * RENG-87 — the KPI strip of the LLM Status page, rendered.
 *
 * The page has no DOM test environment (no jsdom/happy-dom), so the strip is
 * rendered for real through the SSR renderer with a stubbed runtime-health
 * composable: the seven cards, the value each one puts on its face, and the
 * words each one labels it with. That last one is the point — RENG-78 renamed
 * the strip's latency source, the strip kept asking for the renamed key plus a
 * `Window` suffix that no locale carries, and vue-i18n answers a missing key
 * with the key itself, which is how `llm.stats.avgC…` reached the screen. A
 * rendering test sees that; a key-level unit test has to be written to look.
 *
 * The layout half of the contract (the row fills its container, the value is
 * one line, the label clamps at two) is pinned against the stylesheet in
 * `designLanguage.spec.ts` — there is nothing to measure here.
 */

interface StripState {
  providers: Ref<Record<string, unknown>[]>;
  latencyAvailable: Ref<boolean>;
  latencyWindowDays: Ref<number | null>;
  usageAvailable: Ref<boolean>;
  usageWindowDays: Ref<number | null>;
  usageTotal: Ref<number | null>;
}

/** The runtime-health composable the view reads, replaced by a fixture the
 *  tests drive: `__state` holds the same refs the view renders from, so a test
 *  sets the data a page load would have fetched. */
vi.mock('../composables/useLlmStatus', async () => {
  const { ref, computed } = await import('vue');
  const providers = ref<Record<string, unknown>[]>([]);
  const latencyAvailable = ref(false);
  const latencyWindowDays = ref<number | null>(null);
  const usageAvailable = ref(false);
  const usageWindowDays = ref<number | null>(null);
  const usageTotal = ref<number | null>(null);
  const statusCount = (status: string) =>
    computed(() => providers.value.filter((p) => p.status === status).length);
  return {
    useLlmStatus: () => ({
      providers,
      loading: ref(false),
      error: ref<string | null>(null),
      testingId: ref<string | null>(null),
      testResults: new Map(),
      healthyCount: statusCount('healthy'),
      degradedCount: statusCount('degraded'),
      errorCount: statusCount('error'),
      offlineCount: statusCount('offline'),
      latencyAvailable,
      latencyWindowDays,
      usageAvailable,
      usageWindowDays,
      usageTotal,
      test: async () => ({ ok: true }),
      fetch: async () => {},
    }),
    __state: { providers, latencyAvailable, latencyWindowDays, usageAvailable, usageWindowDays, usageTotal },
  };
});

async function state(): Promise<StripState> {
  const mocked = (await import('../composables/useLlmStatus')) as unknown as { __state: StripState };
  return mocked.__state;
}

/** A provider whose card carries a PROBE reading — `stripLatency` takes the
 *  probe's round trip wherever one was sampled (RENG-78), which is the reading
 *  that used to print its own i18n key. */
function probeProvider(over: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    id: 'xiaomi-0',
    name: 'xiaomi',
    status: 'healthy',
    disabled: false,
    avgProbeLatencyMs: 246,
    probeSampleCount: 4,
    avgLatencyMs: null,
    avgTtfbMs: null,
    latencySampleCount: 0,
    ...over,
  };
}

async function renderStrip(locale: 'en' | 'zh-CN'): Promise<string> {
  const i18n = createI18n({
    legacy: false,
    locale,
    messages: { [locale]: locale === 'zh-CN' ? zhCN : en },
  });
  const app = createSSRApp(LlmStatus).use(i18n);
  // Element Plus wants deterministic id and z-index sources during SSR.
  app.provide(ID_INJECTION_KEY, { prefix: 1, current: 0 });
  app.provide(ZINDEX_INJECTION_KEY, { current: 0 });
  return await renderToString(app);
}

/** The text of every element carrying `className`, in render order. The class
 *  attribute also carries Element Plus's own classes and the scoped-style
 *  marker, so the pattern looks for the class among the others. */
function cellText(html: string, className: string): string[] {
  const pattern = new RegExp(`class="[^"]*\\b${className}\\b[^"]*"[^>]*>([^<]*)<`, 'g');
  return [...html.matchAll(pattern)].map((match) => match[1].trim());
}

/** How many elements carry `className` — for wrappers whose content is markup,
 *  where `cellText` has no text to read. */
function countClass(html: string, className: string): number {
  return (html.match(new RegExp(`class="[^"]*\\b${className}\\b[^"]*"`, 'g')) ?? []).length;
}

/** Everything the page shows, tags removed — what a raw i18n key would appear
 *  in. */
const visibleText = (html: string) => html.replace(/<[^>]+>/g, ' ').replace(/\s+/g, ' ');

describe('RENG-87 — the LLM Status KPI strip', () => {
  beforeEach(async () => {
    const strip = await state();
    strip.providers.value = [];
    strip.latencyAvailable.value = false;
    strip.latencyWindowDays.value = null;
    strip.usageAvailable.value = false;
    strip.usageWindowDays.value = null;
    strip.usageTotal.value = null;
  });

  it('renders the seven KPIs, in order, with a value and a label each', async () => {
    const strip = await state();
    strip.providers.value = [probeProvider(), probeProvider({ id: 'b', name: 'ollama', status: 'offline' })];
    strip.latencyAvailable.value = true;
    strip.latencyWindowDays.value = 7;
    strip.usageAvailable.value = true;
    strip.usageWindowDays.value = 7;
    strip.usageTotal.value = 4231;

    const html = await renderStrip('en');

    expect(countClass(html, 'stat-card')).toBe(7);
    expect(cellText(html, 'stat-value')).toEqual(['2', '1', '0', '0', '1', '246ms', '4,231']);
    expect(cellText(html, 'stat-label')).toEqual([
      'Providers',
      'Healthy',
      'Degraded',
      'Error',
      'Offline',
      'Avg comm. latency (last 7 days)',
      'Recorded usages (last 7 days)',
    ]);
  });

  it('labels a probe reading through i18n, not with the key it asks for', async () => {
    const strip = await state();
    strip.providers.value = [probeProvider()];
    strip.latencyAvailable.value = true;
    strip.latencyWindowDays.value = 7;

    for (const [locale, expected] of [
      ['en', 'Avg comm. latency (last 7 days)'],
      ['zh-CN', '平均通信延迟（过去 7 天）'],
    ] as const) {
      const html = await renderStrip(locale);
      const labels = cellText(html, 'stat-label');
      expect(labels[5], locale).toBe(expected);
      // The failure mode RENG-87 shipped: vue-i18n hands back the key path
      // when there is no message for it, and the card shows it verbatim.
      expect(visibleText(html), `${locale} printed an i18n key`).not.toContain('llm.stats.');
      expect(visibleText(html), `${locale} printed an i18n key`).not.toContain('llm.');
    }
  });

  it('writes an unmeasured KPI as a single dash, never as a broken key', async () => {
    const html = await renderStrip('zh-CN');

    const values = cellText(html, 'stat-value');
    expect(values[5]).toBe('—');
    expect(values[6]).toBe('—');
    // No whitespace in a value: nothing can wrap it onto a second line, and
    // the empty state is one token rather than a stacked dash/digit.
    for (const value of values) expect(value).not.toMatch(/\s/);
    // With nothing measured the label drops the window it cannot name.
    expect(cellText(html, 'stat-label')[5]).toBe('平均延迟');
    expect(cellText(html, 'stat-label')[6]).toBe('使用总数');
    expect(visibleText(html)).not.toContain('llm.');
  });
});
