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
  /** Average response latency in milliseconds. */
  latencyMs: number
  /** Error rate as a fraction (0.0–1.0). */
  errorRate: number
  /** Total request count in the current window. */
  requestCount: number
  /** Token usage as a percentage of the quota (if available). */
  usagePercent?: number
  /** Sparkline data points for the usage chart. */
  sparkline?: number[]
  /** ISO 8601 timestamp of the last health check. */
  lastChecked: string
  /** Editable config echoed back by GET /llm/providers (the API key is never returned). */
  apiBaseUrl?: string
  /** Default model for this provider. */
  defaultModel?: string
  /** Maximum tokens per request. */
  maxTokens?: number
  /** Sampling temperature. */
  temperature?: number
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
