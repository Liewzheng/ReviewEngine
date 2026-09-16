/**
 * Health status of an LLM provider. `disabled` (RENG-75) is the
 * administrative off switch, deliberately distinct from `offline`: a
 * disabled card was never probed and its state is not a failure.
 */
export type LlmProviderStatus = 'healthy' | 'degraded' | 'error' | 'offline' | 'disabled'

/** Result of an LLM provider connectivity test. */
export interface TestResult {
  /** Whether the test request succeeded. */
  success: boolean
  /** Round-trip latency in milliseconds. */
  latencyMs?: number
  /** Error message if the test failed. */
  error?: string
  /** ISO 8601 timestamp of the test. */
  timestamp?: string
}

/** An LLM provider as displayed in the LLM Status dashboard. */
export interface LlmProvider {
  /** Unique provider identifier. */
  id: string
  /** Display name (e.g. "OpenAI", "Anthropic"). */
  name: string
  /** Provider logo URL or icon identifier. */
  logo: string
  /** Current health status. */
  status: LlmProviderStatus
  /**
   * RENG-75: the administrative off switch. A disabled provider keeps its
   * config and history but sits outside the review chain and is never probed,
   * so it also carries `chainPosition: null` and `isPrimary: false`.
   */
  disabled: boolean
  /** Whether API credentials are configured. */
  configured: boolean
  /**
   * Round-trip time of the health probe in milliseconds (0 when the provider
   * was not probed, e.g. no API key stored) — the value
   * `GET /api/v1/llm/providers` reports from its probe cache (RENG-36).
   *
   * RENG-57 renamed this from `latencyMs`: it is an INSTANTANEOUS measurement
   * of one `GET /models`, and the card also shows a historical average
   * (`avgLatencyMs`) — the RENG-53 finding was that the two were
   * indistinguishable on screen.
   */
  lastProbeLatencyMs: number
  /**
   * RENG-77: mean communication latency (time to first byte) of the calls
   * recorded for this provider, in whole milliseconds; `null` when no sample
   * carries a TTFB yet.
   *
   * RENG-78 demoted it from the card's primary display: the shipped request
   * shape is non-streaming, so a provider's headers and body arrive together
   * and `avgTtfbMs ≈ avgLatencyMs` (measured, RENG-77 §5) — neither is a
   * network metric. The field stays on the wire for anyone reading the API;
   * `avgProbeLatencyMs` is what the card shows.
   */
  avgTtfbMs: number | null
  /**
   * RENG-78: mean round-trip time of the PROBES over the latency window, in
   * whole milliseconds — one `GET {api_base}/models` each (DNS + TCP + TLS +
   * HTTP), with no model involved anywhere. This is the card's "communication
   * latency" (平均通信延迟); the frontend prefers it and falls back to
   * `avgLatencyMs` when it is `null` (no successful probe recorded / the
   * samples could not be read) — `null` means unknown, never `0`.
   *
   * Failed probes are excluded (a 401 answered in 5 ms or a 120 s timeout is
   * the failure's shape, not the link's) and are still recorded in the sample
   * table; `probeSampleCount` counts the successes behind this mean.
   */
  avgProbeLatencyMs: number | null
  /**
   * RENG-78: successful probes in the window — the denominator of
   * `avgProbeLatencyMs`, a measured count (`0` is a real value); `null` when
   * the probe samples could not be read at all.
   */
  probeSampleCount: number | null
  /**
   * RENG-57: mean round-trip time of the SUCCESSFUL LLM calls this provider
   * served inside the latency window (`latencyWindowDays`), in whole
   * milliseconds; `null` when the window holds no successful call (or the
   * server could not read the samples) — `null` means unknown, never `0`.
   *
   * Failed calls are excluded (a 401 answered in 5 ms or a 120 s timeout
   * measures the failure, not the provider) and counted in
   * `latencyFailureCount` instead.
   */
  avgLatencyMs: number | null
  /**
   * RENG-57: successful calls in the window — the average's denominator, a
   * measured count (0 is a real value). `null` when the samples could not be
   * read at all.
   */
  latencySampleCount: number | null
  /** RENG-57: failed calls in the window, excluded from the average. */
  latencyFailureCount: number | null
  /** RENG-57: ISO 8601 time of the newest recorded call, or `null`. */
  latencyLastSampleAt: string | null
  /**
   * RENG-57: per-bucket mean latency over the window, oldest bucket first
   * (28 six-hour buckets). `null` means the provider has no recorded call in
   * the window — there is no series to draw, and a flat zero line would be a
   * fabrication. Inside a series, a `null` bucket is a gap with no calls.
   */
  latencySparkline: (number | null)[] | null
  /**
   * RENG-56: reviews that recorded this provider inside the usage window
   * (`usageWindowDays`), or `null` when the server could not read the usage
   * history (no store attached / aggregate failed) — `null` means unknown,
   * never zero. The count is review-level: one review counts once per
   * provider, whatever model it used.
   */
  requestCount: number | null
  /**
   * RENG-56: this provider's share of all usage recorded in the window, as a
   * fraction (0.0–1.0); `null` when the window holds no usage at all.
   */
  usageShare: number | null
  /**
   * RENG-56: `completed / (completed + failed)` over the reviews that used
   * this provider in the window, as a fraction (0.0–1.0); `null` when none of
   * them reached an outcome. Reviews that failed before recording any usage
   * carry no snapshot and are invisible here, so this is an upper bound on
   * the provider's call success.
   */
  successRate: number | null
  /** RENG-56: ISO 8601 timestamp of the newest recorded use, or `null`. */
  lastUsedAt: string | null
  /**
   * ISO 8601 timestamp of the probe behind `status`; `null` when the provider
   * was never probed (no report entry) — not "now".
   */
  lastChecked: string | null
  /** Editable config echoed back by GET /llm/providers (the API key is never returned). */
  apiBaseUrl?: string
  /** Default model for this provider. */
  defaultModel?: string
  /** Maximum tokens per request. */
  maxTokens?: number
  /** Sampling temperature. */
  temperature?: number
  /**
   * RENG-55: index of this provider in the STORED list (0-based) — the value
   * the UI array order and `llm_providers.raw.position` both encode. The
   * primary selection does not change it.
   */
  position?: number
  /**
   * RENG-55: 1-based rank in the authoritative runtime chain (the first
   * ENABLED card leads, the rest follow in stored order) — what the cards
   * render as the chain marker. RENG-75: `null` for a disabled provider,
   * which is not in the chain at all.
   */
  chainPosition?: number | null
  /** RENG-55: true for the chain head, i.e. the provider reviews run on first. */
  isPrimary?: boolean
}

/** Supported LLM provider types for the configuration dropdown. */
export const PROVIDER_TYPES = [
  { label: 'OpenAI', value: 'openai' },
  { label: 'Anthropic', value: 'anthropic' },
  { label: 'Ollama', value: 'ollama' },
  { label: 'Google (Gemini)', value: 'google' },
  { label: 'Azure OpenAI', value: 'azure' },
  { label: 'xAI (Grok)', value: 'xai' },
  { label: 'DeepSeek', value: 'deepseek' },
  { label: 'Mistral AI', value: 'mistral' },
  { label: 'Together AI', value: 'togetherai' },
  { label: 'OpenRouter', value: 'openrouter' },
  { label: 'Custom', value: 'custom' },
] as const
