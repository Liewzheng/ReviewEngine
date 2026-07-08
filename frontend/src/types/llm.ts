export type LlmProviderStatus = 'healthy' | 'degraded' | 'error' | 'offline'

export interface TestResult {
  success: boolean
  latencyMs?: number
  error?: string
  timestamp?: string
}

export interface LlmProvider {
  id: string
  name: string
  logo: string
  status: LlmProviderStatus
  configured: boolean
  latencyMs: number
  errorRate: number
  requestCount: number
  usagePercent?: number
  sparkline?: number[]
  lastChecked: string
}

/** Input/update shape for the provider management CRUD endpoints. */
export interface ProviderConfig {
  provider: string
  apiKey: string
  apiBaseUrl: string
  defaultModel?: string
  maxTokens?: number
  temperature?: number
  timeout?: number
  retry?: number
}

/** Response shape returned by add/update/delete provider endpoints. */
export interface ProviderResponse {
  id: string
  provider: string
  apiBaseUrl: string
  defaultModel?: string
  maxTokens?: number
  temperature?: number
  timeout?: number
  retry?: number
  configured: boolean
  status?: LlmProviderStatus
  createdAt?: string
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
