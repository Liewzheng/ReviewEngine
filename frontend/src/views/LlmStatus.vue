<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted } from 'vue'
import { ElNotification } from 'element-plus'
import { RefreshRight, Cpu, CircleCheck, Warning, CircleClose, Remove } from '@element-plus/icons-vue'
import ProviderCard from '../components/LlmStatus/ProviderCard.vue'
import type { LlmProvider, TestResult } from '../types/llm'

/* ------------------------------------------------------------------ */
/*  Mock Data                                                         */
/* ------------------------------------------------------------------ */

function generateSparkline(base: number, variance: number): number[] {
  return Array.from({ length: 24 }, (_, i) => {
    const trend = Math.sin(i / 3) * variance * 0.5
    const noise = (Math.random() - 0.5) * variance
    return Math.max(10, Math.round(base + trend + noise))
  })
}

function createMockProviders(): LlmProvider[] {
  const now = new Date().toISOString()
  return [
    {
      id: 'openai',
      name: 'OpenAI',
      logo: '🅾️',
      status: 'healthy',
      latencyMs: 234,
      requestCount: 1204,
      errorRate: 0.002,
      usagePercent: 74,
      lastChecked: now,
      configured: true,
      sparkline: generateSparkline(230, 60),
    },
    {
      id: 'anthropic',
      name: 'Anthropic',
      logo: '🅰️',
      status: 'healthy',
      latencyMs: 189,
      requestCount: 856,
      errorRate: 0.001,
      usagePercent: 45,
      lastChecked: now,
      configured: true,
      sparkline: generateSparkline(190, 40),
    },
    {
      id: 'ollama',
      name: 'Ollama Local',
      logo: '🦙',
      status: 'degraded',
      latencyMs: 1200,
      requestCount: 320,
      errorRate: 0.025,
      usagePercent: 91,
      lastChecked: now,
      configured: true,
      sparkline: generateSparkline(1150, 200),
    },
    {
      id: 'gemini',
      name: 'Google Gemini',
      logo: '♊',
      status: 'offline',
      latencyMs: 0,
      requestCount: 0,
      errorRate: 0,
      usagePercent: undefined,
      lastChecked: now,
      configured: false,
      sparkline: undefined,
    },
    {
      id: 'azure',
      name: 'Azure OpenAI',
      logo: '☁️',
      status: 'error',
      latencyMs: 0,
      requestCount: 0,
      errorRate: 0,
      usagePercent: undefined,
      lastChecked: now,
      configured: true,
      sparkline: undefined,
    },
    {
      id: 'groq',
      name: 'Groq',
      logo: '⚡',
      status: 'healthy',
      latencyMs: 156,
      requestCount: 2100,
      errorRate: 0.0005,
      usagePercent: 38,
      lastChecked: now,
      configured: true,
      sparkline: generateSparkline(150, 30),
    },
  ]
}

/* ------------------------------------------------------------------ */
/*  State                                                             */
/* ------------------------------------------------------------------ */

const providers = ref<LlmProvider[]>([])
const loading = ref(true)
const testingMap = ref<Record<string, boolean>>({})
const cardRefs = ref<InstanceType<typeof ProviderCard>[]>([])

const healthyCount = computed(() => providers.value.filter(p => p.status === 'healthy').length)
const degradedCount = computed(() => providers.value.filter(p => p.status === 'degraded').length)
const errorCount = computed(() => providers.value.filter(p => p.status === 'error').length)
const offlineCount = computed(() => providers.value.filter(p => p.status === 'offline').length)

const avgLatency = computed(() => {
  const active = providers.value.filter(p => p.configured && p.status !== 'offline' && p.latencyMs > 0)
  if (!active.length) return 0
  return Math.round(active.reduce((sum, p) => sum + p.latencyMs, 0) / active.length)
})

const totalRequests = computed(() =>
  providers.value.reduce((sum, p) => sum + p.requestCount, 0)
)

/* ------------------------------------------------------------------ */
/*  Actions                                                           */
/* ------------------------------------------------------------------ */

function setLoading(val: boolean) {
  loading.value = val
}

function fetchProviders() {
  setLoading(true)
  // Simulate API delay
  setTimeout(() => {
    providers.value = createMockProviders()
    setLoading(false)
  }, 800)
}

function handleRefreshAll() {
  setLoading(true)
  // Simulate bulk test API
  setTimeout(() => {
    providers.value = createMockProviders().map(p => ({
      ...p,
      lastChecked: new Date().toISOString(),
      // Slightly jitter values for realism
      latencyMs: p.configured && p.status !== 'offline'
        ? Math.max(10, p.latencyMs + Math.round((Math.random() - 0.5) * 40))
        : p.latencyMs,
    }))
    setLoading(false)

    const healthy = providers.value.filter(p => p.status === 'healthy').length
    const issues = providers.value.filter(p => p.status === 'degraded' || p.status === 'error').length

    ElNotification({
      title: 'Providers Refreshed',
      message: `All providers tested — ${healthy} healthy, ${issues} issues`,
      type: issues === 0 ? 'success' : 'warning',
      duration: issues === 0 ? 3000 : 5000,
    })
  }, 1200)
}

function handleTestSingle(provider: LlmProvider) {
  testingMap.value[provider.id] = true

  // Simulate per-provider test API
  setTimeout(() => {
    testingMap.value[provider.id] = false

    const success = provider.status !== 'error' && provider.status !== 'offline'
    const result: TestResult = {
      success,
      latencyMs: success ? Math.max(10, provider.latencyMs + Math.round((Math.random() - 0.5) * 30)) : undefined,
      error: success ? undefined : 'Connection timeout after 10s. Check API key and network.',
      timestamp: new Date().toISOString(),
    }

    // Find the matching card ref and show result
    const card = cardRefs.value.find(c => c.providerId === provider.id)
    if (card) {
      card.showTestResult(result)
    }
  }, 800)
}

/* ------------------------------------------------------------------ */
/*  Lifecycle                                                         */
/* ------------------------------------------------------------------ */

onMounted(() => {
  fetchProviders()
})

onUnmounted(() => {
  // cleanup if any
})
</script>

<template>
  <div class="llm-page">
    <!-- Page Header -->
    <div class="page-header">
      <div class="header-text">
        <h2 class="page-title">LLM Status</h2>
        <p class="page-subtitle">Provider health and performance</p>
      </div>
      <el-button
        type="primary"
        :icon="RefreshRight"
        :loading="loading"
        @click="handleRefreshAll"
      >
        Refresh All
      </el-button>
    </div>

    <!-- Loading Skeleton -->
    <div v-if="loading && providers.length === 0" class="skeleton-grid">
      <el-skeleton
        v-for="i in 6"
        :key="i"
        animated
        :rows="4"
        class="skeleton-card"
      />
    </div>

    <!-- Empty State -->
    <el-empty
      v-else-if="providers.length === 0"
      description="No providers configured"
    >
      <el-button type="primary" @click="fetchProviders">Reload</el-button>
    </el-empty>

    <!-- Content -->
    <template v-else>
      <!-- Summary Stats -->
      <div class="stats-row">
        <el-card shadow="never" class="stat-card">
          <div class="stat-content">
            <el-icon class="stat-icon" :size="24"><Cpu /></el-icon>
            <div class="stat-body">
              <div class="stat-value">{{ providers.length }}</div>
              <div class="stat-label">Providers</div>
            </div>
          </div>
        </el-card>
        <el-card shadow="never" class="stat-card">
          <div class="stat-content">
            <el-icon class="stat-icon" :size="24" color="var(--success)"><CircleCheck /></el-icon>
            <div class="stat-body">
              <div class="stat-value" style="color: var(--success)">{{ healthyCount }}</div>
              <div class="stat-label">Healthy</div>
            </div>
          </div>
        </el-card>
        <el-card shadow="never" class="stat-card">
          <div class="stat-content">
            <el-icon class="stat-icon" :size="24" color="var(--warning)"><Warning /></el-icon>
            <div class="stat-body">
              <div class="stat-value" style="color: var(--warning)">{{ degradedCount }}</div>
              <div class="stat-label">Degraded</div>
            </div>
          </div>
        </el-card>
        <el-card shadow="never" class="stat-card">
          <div class="stat-content">
            <el-icon class="stat-icon" :size="24" color="var(--error)"><CircleClose /></el-icon>
            <div class="stat-body">
              <div class="stat-value" style="color: var(--error)">{{ errorCount }}</div>
              <div class="stat-label">Error</div>
            </div>
          </div>
        </el-card>
        <el-card shadow="never" class="stat-card">
          <div class="stat-content">
            <el-icon class="stat-icon" :size="24" color="var(--offline)"><Remove /></el-icon>
            <div class="stat-body">
              <div class="stat-value" style="color: var(--offline)">{{ offlineCount }}</div>
              <div class="stat-label">Offline</div>
            </div>
          </div>
        </el-card>
        <el-card shadow="never" class="stat-card">
          <div class="stat-content">
            <el-icon class="stat-icon" :size="24"><RefreshRight /></el-icon>
            <div class="stat-body">
              <div class="stat-value">{{ avgLatency }} ms</div>
              <div class="stat-label">Avg Latency</div>
            </div>
          </div>
        </el-card>
        <el-card shadow="never" class="stat-card">
          <div class="stat-content">
            <el-icon class="stat-icon" :size="24"><Cpu /></el-icon>
            <div class="stat-body">
              <div class="stat-value">{{ new Intl.NumberFormat('en-US').format(totalRequests) }}</div>
              <div class="stat-label">Total Requests</div>
            </div>
          </div>
        </el-card>
      </div>

      <!-- Provider Grid -->
      <div class="provider-grid">
        <ProviderCard
          v-for="(provider, idx) in providers"
          :key="provider.id"
          ref="cardRefs"
          :provider="provider"
          :index="idx"
          :testing="testingMap[provider.id]"
          :loading="loading"
          @test="handleTestSingle"
        />
      </div>
    </template>
  </div>
</template>

<style scoped>
.llm-page {
  max-width: 1400px;
  margin: 0 auto;
}

.page-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  margin-bottom: 24px;
  flex-wrap: wrap;
}

.header-text {
  flex: 1;
}

.page-title {
  font-size: 24px;
  font-weight: 600;
  color: var(--text-primary);
  margin-bottom: 4px;
  letter-spacing: -0.02em;
  line-height: 1.3;
}

.page-subtitle {
  font-size: 14px;
  color: var(--text-secondary);
  margin: 0;
}

/* Stats Row */
.stats-row {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(160px, 1fr));
  gap: 12px;
  margin-bottom: 24px;
}

.stat-card :deep(.el-card__body) {
  padding: 16px;
}

.stat-content {
  display: flex;
  align-items: center;
  gap: 12px;
}

.stat-icon {
  color: var(--text-secondary);
  flex-shrink: 0;
}

.stat-body {
  flex: 1;
  min-width: 0;
}

.stat-value {
  font-family: var(--font-mono);
  font-size: 20px;
  font-weight: 600;
  color: var(--text-primary);
  line-height: 1.2;
}

.stat-label {
  font-size: 12px;
  color: var(--text-secondary);
  margin-top: 2px;
}

/* Skeleton */
.skeleton-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(320px, 1fr));
  gap: 16px;
}

.skeleton-card {
  padding: 20px;
  background: var(--bg-card);
  border-radius: var(--radius-md);
  border: 1px solid var(--border-color);
}

/* Provider Grid */
.provider-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(320px, 1fr));
  gap: 16px;
}

/* Responsive */
@media (max-width: 768px) {
  .page-header {
    flex-direction: column;
    align-items: stretch;
  }

  .stats-row {
    grid-template-columns: repeat(2, 1fr);
  }

  .provider-grid {
    grid-template-columns: 1fr;
  }

  .skeleton-grid {
    grid-template-columns: 1fr;
  }
}

@media (min-width: 769px) and (max-width: 1024px) {
  .provider-grid {
    grid-template-columns: repeat(2, 1fr);
  }

  .skeleton-grid {
    grid-template-columns: repeat(2, 1fr);
  }
}

@media (min-width: 1025px) and (max-width: 1279px) {
  .provider-grid {
    grid-template-columns: repeat(3, 1fr);
  }

  .skeleton-grid {
    grid-template-columns: repeat(3, 1fr);
  }
}

@media (min-width: 1280px) {
  .provider-grid {
    grid-template-columns: repeat(4, 1fr);
  }

  .skeleton-grid {
    grid-template-columns: repeat(4, 1fr);
  }
}
</style>
