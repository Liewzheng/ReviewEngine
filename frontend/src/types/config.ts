/** GitLab integration configuration for webhook and MR review triggers. */
export interface GitLabConfig {
  /** GitLab instance base URL (e.g. `https://gitlab.example.com`). */
  url: string
  /** Personal access token for GitLab API authentication. */
  apiToken: string
  /** Webhook secret for validating incoming webhook payloads. */
  webhookSecret: string
  /** HMAC signing secret for webhook signature verification. */
  webhookSigningSecret: string
  /** Default project path (e.g. `group/project`) for auto-review triggers. */
  defaultProject: string
  /** MR label that triggers automatic review when applied. */
  mrLabel: string
  /** Whether to automatically review new/updated MRs. */
  autoReview: boolean
}

/** LLM provider configuration for code review AI models. */
export interface LLMConfig {
  /** Base URL for the LLM API (e.g. `https://api.openai.com/v1`). */
  apiBaseUrl: string
  /** API key for authentication with the LLM provider. */
  openaiApiKey: string
  /** Default model identifier (e.g. `gpt-4o`, `claude-3-opus`). */
  defaultModel: string
  /** Maximum tokens in the LLM response. */
  maxTokens: number
  /** Sampling temperature (0.0 = deterministic, 1.0 = creative). */
  temperature: number
  /** Request timeout in seconds before the LLM call is retried. */
  timeoutSeconds: number
  /** Number of retry attempts on transient LLM API failures. */
  retryAttempts: number
}

/** Review quality rules and gating configuration. */
export interface ReviewRules {
  /** Minimum overall score (0–100) for a review to pass. */
  minScore: number
  /** Whether to block MR merge when critical findings are present. */
  blockOnCritical: boolean
  /** Whether to automatically post review comments on passing reviews. */
  autoCommentOnPass: boolean
  /** Template string for auto-posted review comments (supports `{{score}}` and `{{summary}}`). */
  commentTemplate: string
  /** Glob patterns for files excluded from review (e.g. `*.lock`, `node_modules/**`). */
  excludedPatterns: string[]
  /** Expert names that must run for every review (empty = all enabled). */
  requiredExperts: string[]
  /** Maximum allowed review duration in seconds before timeout. */
  maxReviewDurationSeconds: number
}

/** Advanced server and runtime configuration. */
export interface AdvancedConfig {
  /** Server log level: `debug`, `info`, `warn`, or `error`. */
  logLevel: 'debug' | 'info' | 'warn' | 'error'
  /** Number of days to retain log entries before automatic cleanup. */
  logRetentionDays: number
  /** SSE heartbeat interval in seconds (prevents proxy timeout). */
  sseHeartbeatInterval: number
  /** Maximum number of reviews that can run concurrently. */
  maxConcurrentReviews: number
  /** HTTP request timeout in seconds for all API calls. */
  requestTimeout: number
  /** Whether to expose Prometheus metrics endpoint. */
  enableMetrics: boolean
  /** Whether to enable verbose debug logging and raw LLM response dumps. */
  debugMode: boolean
}

/** Complete application configuration combining all config sections. */
export interface AppConfig {
  /** GitLab integration settings. */
  gitlab: GitLabConfig
  /** LLM provider settings. */
  llm: LLMConfig
  /** Review quality rules. */
  rules: ReviewRules
  /** Advanced runtime settings. */
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

/** Result of an LLM connection test. */
export interface TestResult {
  /** Whether the connection test succeeded. */
  success: boolean
  /** Round-trip latency in milliseconds (if measured). */
  latencyMs?: number
  /** Error message if the test failed. */
  error?: string
  /** ISO 8601 timestamp when the test was performed. */
  timestamp: string
}

/**
 * Create a mock `AppConfig` for development and testing.
 * Returns a fully-populated config with placeholder credentials.
 */
export function createMockConfig(): AppConfig {
  return {
    gitlab: {
      url: 'https://gitlab.example.com',
      apiToken: 'glpat-xxxxxxxxxxxxxxxxxxxx',
      webhookSecret: 'whsec-xxxxxxxxxxxxxxxx',
      webhookSigningSecret: 'whsec-sign-xxxxxxxxxxxxxxxx',
      defaultProject: '',
      mrLabel: 'needs-review',
      autoReview: true,
    },
    llm: {
      apiBaseUrl: 'https://api.openai.com/v1',
      openaiApiKey: 'sk-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx',
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
