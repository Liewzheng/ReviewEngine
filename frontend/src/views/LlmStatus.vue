<script setup lang="ts">
import { ref, computed, watch, onMounted } from 'vue'
import { ElNotification } from 'element-plus'
import { RefreshRight, Cpu, CircleCheck, Warning, CircleClose, Remove } from '@element-plus/icons-vue'
import { useLlmStatus } from '../composables/useLlmStatus'
import ProviderCard from '../components/LlmStatus/ProviderCard.vue'
import type { LlmProvider } from '../types/llm'

/* ------------------------------------------------------------------ */
/*  Composable                                                        */
/* ------------------------------------------------------------------ */

const llm = useLlmStatus()

const providers = llm.providers
const loading = llm.loading
const cardRefs = ref<InstanceType<typeof ProviderCard>[]>([])

const testingMap = computed<Record<string, boolean>>(() => {
  if (!llm.testingId.value) return {}
  return { [llm.testingId.value]: true }
})

const healthyCount = computed(() => llm.healthyCount.value)
const degradedCount = computed(() => llm.degradedCount.value)
const errorCount = computed(() => llm.errorCount.value)
const offlineCount = computed(() => llm.offlineCount.value)

const avgLatency = computed(() => {
  const active = providers.value.filter(p => p.configured && p.status !== 'offline' && p.latencyMs > 0)
  if (!active.length) return 0
  return Math.round(active.reduce((sum, p) => sum + p.latencyMs, 0) / active.length)
})

const totalRequests = computed(() =>
  providers.value.reduce((sum, p) => sum + p.requestCount, 0)
)

/* ------------------------------------------------------------------ */
/*  Error Handling                                                    */
/* ------------------------------------------------------------------ */

watch(() => llm.error.value, (err) => {
  if (err) {
    ElNotification({
      title: 'Error',
      message: err,
      type: 'error',
      duration: 5000,
    })
  }
})

/* ------------------------------------------------------------------ */
/*  Actions                                                           */
/* ------------------------------------------------------------------ */

function fetchProviders() {
  llm.fetch()
}

function handleRefreshAll() {
  llm.fetch().then(() => {
    const healthy = healthyCount.value
    const issues = degradedCount.value + errorCount.value

    ElNotification({
      title: 'Providers Refreshed',
      message: `All providers tested — ${healthy} healthy, ${issues} issues`,
      type: issues === 0 ? 'success' : 'warning',
      duration: issues === 0 ? 3000 : 5000,
    })
  })
}

async function handleTestSingle(provider: LlmProvider) {
  try {
    const result = await llm.test(provider.id)
    const card = cardRefs.value.find(c => c.providerId === provider.id)
    if (card) {
      card.showTestResult(result)
    }
  } catch {
    // Error already handled by composable
  }
}

/* ------------------------------------------------------------------ */
/*  Lifecycle                                                         */
/* ------------------------------------------------------------------ */

onMounted(() => {
  fetchProviders()
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
