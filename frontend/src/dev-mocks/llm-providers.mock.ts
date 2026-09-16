/**
 * Mock LLM provider cards (RENG-75) — one card per visual state the
 * redesigned card has to render, so the layout can be checked without
 * arranging four real accounts behind the backend:
 *
 *  - healthy (chain head, full statistics)
 *  - healthy secondary (a second account under its own name)
 *  - degraded (slow, partial success — amber stripe + amber dot)
 *  - errored (red stripe + red dot + probe failure reason in the footer)
 *  - disabled (dimmed, no coloured decoration, `已停用` footer)
 *
 * Both halves of the page are mocked: the runtime health list
 * (`GET /llm/providers`), the `/system/health` rows that carry the probe's own
 * failure text, and the config echo (`GET /config` `llm.providers[]`).
 *
 * Enabled only when `VITE_USE_LLM_MOCKS=true` at build time (see
 * `llmMocksEnabled`); production and normal preview builds never reach this
 * module, because the flag is a static `import.meta.env` read that the
 * bundler folds away together with the dynamic import behind it.
 */
import type { LlmProvider } from '../types/llm';
import type { ProviderCardState } from '../composables/llmPayload';
import type { HealthStatus } from '../types/dashboard';

const now = Date.now();
const iso = (offsetMs: number) => new Date(now - offsetMs).toISOString();

/** 28 six-hour buckets, the last few carrying the given samples. */
function sparkline(samples: number[]): (number | null)[] {
  const out: (number | null)[] = Array(28).fill(null);
  samples.slice(-28).forEach((value, i) => {
    out[out.length - samples.length + i] = value;
  });
  return out;
}

const base = {
  logo: '',
  configured: true,
  maxTokens: 4096,
  temperature: 0.7,
};

export const MOCK_PROVIDERS: LlmProvider[] = [
  {
    ...base,
    id: 'xiaomi-0',
    name: 'xiaomi',
    status: 'healthy',
    disabled: false,
    lastProbeLatencyMs: 35,
    avgLatencyMs: 300,
    avgTtfbMs: 280,
    latencySampleCount: 7,
    latencyFailureCount: 0,
    latencyLastSampleAt: iso(30 * 60_000),
    latencySparkline: sparkline([280, 295, 300, 305, 290, 300, 300]),
    requestCount: 7,
    usageShare: 0.253,
    successRate: 1,
    lastUsedAt: iso(15 * 60_000),
    lastChecked: iso(60_000),
    apiBaseUrl: 'https://token-plan-cn.api.xiaomimimo.com/v1',
    defaultModel: 'mimo-v2.5',
    position: 0,
    chainPosition: 1,
    isPrimary: true,
  },
  {
    ...base,
    id: 'ollama-1',
    name: 'ollama',
    status: 'healthy',
    disabled: false,
    lastProbeLatencyMs: 12,
    avgLatencyMs: 45,
    avgTtfbMs: 42,
    latencySampleCount: 210,
    latencyFailureCount: 1,
    latencyLastSampleAt: iso(2 * 60_000),
    latencySparkline: sparkline([40, 42, 44, 45, 47, 45]),
    requestCount: 210,
    usageShare: 0.521,
    successRate: 0.995,
    lastUsedAt: iso(5 * 60_000),
    lastChecked: iso(60_000),
    apiBaseUrl: 'http://localhost:11434',
    defaultModel: 'llama3.3',
    position: 1,
    chainPosition: 2,
    isPrimary: false,
  },
  {
    ...base,
    id: 'deepseek-2',
    name: 'deepseek',
    status: 'degraded',
    disabled: false,
    lastProbeLatencyMs: 155,
    avgLatencyMs: 1800,
    avgTtfbMs: 17690,
    latencySampleCount: 42,
    latencyFailureCount: 6,
    latencyLastSampleAt: iso(8 * 60_000),
    latencySparkline: sparkline([260, 300, 900, 1800, 1900, 1750]),
    requestCount: 42,
    usageShare: 0.226,
    successRate: 0.964,
    lastUsedAt: iso(8 * 60_000),
    lastChecked: iso(60_000),
    apiBaseUrl: 'https://api.deepseek.example/v1',
    defaultModel: 'deepseek-v4-flash',
    position: 2,
    chainPosition: 3,
    isPrimary: false,
  },
  {
    ...base,
    id: 'anthropic-3',
    name: 'anthropic',
    status: 'error',
    disabled: false,
    lastProbeLatencyMs: 401,
    avgLatencyMs: null,
    avgTtfbMs: null,
    latencySampleCount: 0,
    latencyFailureCount: 4,
    latencyLastSampleAt: null,
    latencySparkline: sparkline([401, 401, 401]),
    requestCount: 128,
    usageShare: null,
    successRate: 0.821,
    lastUsedAt: iso(2 * 60 * 60_000),
    lastChecked: iso(45_000),
    apiBaseUrl: 'https://api.anthropic.com',
    defaultModel: 'claude-sonnet-4-5',
    position: 3,
    chainPosition: 4,
    isPrimary: false,
  },
  {
    ...base,
    id: 'gemini-4',
    name: 'gemini',
    status: 'disabled',
    disabled: true,
    lastProbeLatencyMs: 0,
    avgLatencyMs: null,
    avgTtfbMs: null,
    latencySampleCount: null,
    latencyFailureCount: null,
    latencyLastSampleAt: null,
    latencySparkline: null,
    requestCount: null,
    usageShare: null,
    successRate: null,
    lastUsedAt: null,
    lastChecked: null,
    apiBaseUrl: 'https://generativelanguage.googleapis.com',
    defaultModel: 'gemini-3-pro',
    position: 4,
    chainPosition: null,
    isPrimary: false,
  },
];

/** The `/system/health` rows the page reads probe failure text from. */
export const MOCK_HEALTH_ROWS: HealthStatus[] = [
  { service: 'xiaomi mimo-v2.5', type: 'llm', status: 'success', message: 'Configured' },
  { service: 'ollama llama3.3', type: 'llm', status: 'success', message: 'Configured' },
  { service: 'deepseek deepseek-v4-flash', type: 'llm', status: 'warning', message: 'Slow response' },
  {
    service: 'anthropic claude-sonnet-4-5',
    type: 'llm',
    status: 'error',
    message: 'HTTP 401 Unauthorized',
  },
  { service: 'gemini gemini-3-pro', type: 'llm', status: 'offline', message: 'Disabled' },
];

/** A card as `GET /config` echoes it: the key is always the `***` sentinel
 *  (the secret never leaves the server), disabled or not. */
function card(
  provider: string,
  apiBaseUrl: string,
  defaultModel: string,
  disabled = false,
  disableThinking = false,
): ProviderCardState {
  return {
    provider,
    apiKey: '***',
    apiBaseUrl,
    defaultModel,
    maxTokens: 4096,
    temperature: 0.7,
    timeoutSeconds: 60,
    retryAttempts: 3,
    disabled,
    disableThinking,
  };
}

/** The card the edit dialog is opened on for the reasoning-model case: the
 *  UAT's `deepseek-v4-flash`, which answers nothing unless the switch is on. */
export const MOCK_CARDS: ProviderCardState[] = MOCK_PROVIDERS.map((p) =>
  card(p.name, p.apiBaseUrl ?? '', p.defaultModel ?? '', p.disabled, p.name === 'deepseek'),
);

export const MOCK_PRIMARY = 'xiaomi';

/** Whether the build asked for the deterministic provider surface. */
export function llmMocksEnabled(): boolean {
  return import.meta.env.VITE_USE_LLM_MOCKS === 'true';
}
