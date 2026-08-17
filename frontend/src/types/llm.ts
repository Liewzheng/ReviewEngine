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

/** Input/update shape for the provider management CRUD endpoints. */
export interface ProviderConfig {
  /** Provider type identifier (e.g. `openai`, `anthropic`). */
  provider: string
  /** API key for the provider. */
  apiKey: string
  /** Base URL for the provider API. */
  apiBaseUrl: string
  /** Default model for this provider. */
  defaultModel?: string
  /** Maximum tokens per request. */
  maxTokens?: number
  /** Sampling temperature. */
  temperature?: number
  /** Request timeout in seconds. */
  timeout?: number
  /** Number of retry attempts on failure. */
  retry?: number
}

/** Response shape returned by add/update/delete provider endpoints. */
export interface ProviderResponse {
  /** Server-assigned provider ID. */
  id: string
  /** Provider type identifier. */
  provider: string
  /** Base URL for the provider API. */
  apiBaseUrl: string
  /** Default model for this provider. */
  defaultModel?: string
  /** Maximum tokens per request. */
  maxTokens?: number
  /** Sampling temperature. */
  temperature?: number
  /** Request timeout in seconds. */
  timeout?: number
  /** Number of retry attempts on failure. */
  retry?: number
  /** Whether the provider has valid credentials configured. */
  configured: boolean
  /** Current health status (if known). */
  status?: LlmProviderStatus
  /** ISO 8601 timestamp when this provider was created. */
  createdAt?: string
  /** ISO 8601 timestamp of the last update. */
  updatedAt?: string
}

/** A provider entry used locally in the Configuration UI (merges input + id tracking). */
export interface ProviderEntry extends ProviderConfig {
  /** Server-assigned id (absent for newly-added, unsaved providers). */
  id?: string
  /** Client-side stable key for v-for rendering. */
  _key: string
  /** Whether the inline edit form is expanded. */
  _expanded: boolean
  /** True when this provider was added but not yet persisted. */
  _isNew?: boolean
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
