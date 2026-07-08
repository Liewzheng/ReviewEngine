export interface GitLabConfig {
  url: string
  apiToken: string
  webhookSecret: string
  webhookSigningSecret: string
  defaultProject: string
  mrLabel: string
  autoReview: boolean
}

export interface LLMConfig {
  primaryProvider: string
  openaiApiKey: string
  anthropicApiKey: string
  ollamaUrl: string
  defaultModel: string
  maxTokens: number
  temperature: number
  timeoutSeconds: number
  retryAttempts: number
}

export interface ReviewRules {
  minScore: number
  blockOnCritical: boolean
  autoCommentOnPass: boolean
  commentTemplate: string
  excludedPatterns: string[]
  requiredExperts: string[]
  maxReviewDurationSeconds: number
}

export interface AdvancedConfig {
  logLevel: 'debug' | 'info' | 'warn' | 'error'
  logRetentionDays: number
  sseHeartbeatInterval: number
  maxConcurrentReviews: number
  requestTimeout: number
  enableMetrics: boolean
  debugMode: boolean
}

export interface AppConfig {
  gitlab: GitLabConfig
  llm: LLMConfig
  rules: ReviewRules
  advanced: AdvancedConfig
  /** Optional server-side metadata (populated when reading from backend). */
  version?: string
  /** Optional expert summary (populated when reading from backend). */
  experts?: { name: string; role: string; title: string; trigger: string; enabled: boolean }[]
  /** Optional command toggles (populated when reading from backend). */
  commands?: Record<string, boolean>
  /** Optional max team size (populated when reading from backend). */
  maxTeamSize?: number
  /** Optional max concurrent LLM calls (populated when reading from backend). */
  maxConcurrentLlmCalls?: number
}

export interface TestResult {
  success: boolean
  latencyMs?: number
  error?: string
  timestamp: string
}

export const providerModels: Record<string, string[]> = {
  openai: ['gpt-4o', 'gpt-4o-mini', 'gpt-4-turbo', 'gpt-3.5-turbo'],
  anthropic: ['claude-3-5-sonnet-20241022', 'claude-3-opus-20240229', 'claude-3-haiku-20240307'],
  ollama: ['llama3.1', 'codellama', 'mistral', 'phi3'],
}

export function createMockConfig(): AppConfig {
  return {
    gitlab: {
      url: 'https://gitlab.example.com',
      apiToken: 'glpat-xxxxxxxxxxxxxxxxxxxx',
      webhookSecret: 'whsec-xxxxxxxxxxxxxxxx',
      webhookSigningSecret: 'whsec-sign-xxxxxxxxxxxxxxxx',
      defaultProject: 'my-group/my-project',
      mrLabel: 'needs-review',
      autoReview: true,
    },
    llm: {
      primaryProvider: 'openai',
      openaiApiKey: 'sk-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx',
      anthropicApiKey: '',
      ollamaUrl: 'http://localhost:11434',
      defaultModel: 'gpt-4o',
      maxTokens: 4096,
      temperature: 0.7,
      timeoutSeconds: 60,
      retryAttempts: 3,
    },
    rules: {
      minScore: 75,
      blockOnCritical: true,
      autoCommentOnPass: true,
      commentTemplate: 'Code review completed. Overall score: {{score}}/100. {{summary}}',
      excludedPatterns: ['*.lock', 'node_modules/**', 'vendor/**', 'dist/**'],
      requiredExperts: ['Security', 'Performance', 'Quality'],
      maxReviewDurationSeconds: 300,
    },
    advanced: {
      logLevel: 'info',
      logRetentionDays: 30,
      sseHeartbeatInterval: 15,
      maxConcurrentReviews: 5,
      requestTimeout: 120,
      enableMetrics: true,
      debugMode: false,
    },
  }
}
