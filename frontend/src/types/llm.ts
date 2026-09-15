/** Health status of an LLM provider. */
export type LlmProviderStatus = 'healthy' | 'degraded' | 'error' | 'offline'

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
  /** Whether API credentials are configured. */
  configured: boolean
  /**
   * Round-trip time of the last health probe in milliseconds (0 when the
   * provider was not probed, e.g. no API key stored) — the value
   * `GET /api/v1/llm/providers` reports from its probe cache (RENG-36).
   */
  latencyMs: number
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
   * RENG-55: 1-based rank in the authoritative runtime chain (primary first,
   * then the stored order) — what the cards render as the chain marker.
   */
  chainPosition?: number
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
