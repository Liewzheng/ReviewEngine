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
