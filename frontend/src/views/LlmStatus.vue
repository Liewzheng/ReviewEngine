<script setup lang="ts">
import { ref, computed, watch, onMounted, onUnmounted } from 'vue'
import { ElMessage, ElNotification } from 'element-plus'
import { useI18n } from 'vue-i18n'
import { RefreshRight, Cpu, CircleCheck, Warning, CircleClose, Remove, Plus } from '@element-plus/icons-vue'
import { useLlmStatus } from '../composables/useLlmStatus'
import { useProviderCards } from '../composables/useProviderCards'
import type { ProviderCardState } from '../composables/llmPayload'
import { getSystemHealth } from '../services/health'
import ProviderConfigCard from '../components/Config/ProviderConfigCard.vue'
import ProviderEditDialog from '../components/Config/ProviderEditDialog.vue'
import type { LlmProvider } from '../types/llm'

/* ------------------------------------------------------------------ */
/*  Runtime health (KPIs + per-card metrics)                           */
/* ------------------------------------------------------------------ */

const { t } = useI18n()
const llm = useLlmStatus()

const providers = llm.providers

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
/*  LLM Configuration — unified provider cards with immediate saves   */
/* ------------------------------------------------------------------ */

/** True when the server reports no usable LLM via /system/health. */
const llmNotConfigured = ref(false)

/** Refresh the not-configured banner. Fail-open: a health-check error keeps
 *  the current banner state. */
function checkLlmConfigured() {
  getSystemHealth()
    .then((health) => {
      llmNotConfigured.value = health.llmConfigured === false
    })
    .catch(() => {})
}

const {
  cards: providerCards,
  primaryName,
  loading: cardsLoading,
  saving: cardsSaving,
  error: cardsError,
  load: loadProviderCards,
  addCard,
  editCard,
  setPrimary,
  deleteCard,
} = useProviderCards({
  statusProviders: llm.providers,
  afterSave: () => {
    // Reflect the new config in the health cards and the banner.
    llm.fetch()
    checkLlmConfigured()
  },
})

/** Runtime health entries keyed by provider name, for the config cards. */
const healthByName = computed(() => {
  const map = new Map<string, LlmProvider>()
  for (const p of providers.value) {
    if (!map.has(p.name)) map.set(p.name, p)
  }
  return map
})

// --- Add/Edit dialog ---
const dialogVisible = ref(false)
const dialogMode = ref<'add' | 'edit'>('add')
const editingCard = ref<ProviderCardState | null>(null)

function openAddDialog() {
  dialogMode.value = 'add'
  editingCard.value = null
  dialogVisible.value = true
}

function openEditDialog(card: ProviderCardState) {
  dialogMode.value = 'edit'
  editingCard.value = { ...card }
  dialogVisible.value = true
}

async function handleDialogSave(form: ProviderCardState) {
  const ok =
    dialogMode.value === 'add'
      ? await addCard(form)
      : await editCard(editingCard.value?.provider ?? form.provider, form)
  if (ok) dialogVisible.value = false
}

/** Card-level connectivity test rides the server-side probe (stored key),
 *  so no secret ever round-trips through the browser. */
async function handleCardTest(card: ProviderCardState) {
  const health = healthByName.value.get(card.provider)
  if (!health) return
  try {
    const result = await llm.test(health.id)
    ElMessage({
      type: result.success ? 'success' : 'error',
      message: result.success
        ? t('config.llm.connected', { n: result.latencyMs })
        : t('config.llm.testFailed', { error: result.error }),
    })
  } catch {
    // Error already handled by composable (llm.error watcher notifies).
  }
}

function isCardTesting(card: ProviderCardState): boolean {
  const health = healthByName.value.get(card.provider)
  return !!health && llm.testingId.value === health.id
}

/* ------------------------------------------------------------------ */
/*  Error Handling                                                    */
/* ------------------------------------------------------------------ */

watch(() => llm.error.value, (err) => {
  if (err) {
    ElNotification({
      title: t('common.error'),
      message: err,
      type: 'error',
      duration: 5000,
    })
  }
})

watch(() => cardsError.value, (err) => {
  if (err) {
    ElNotification({
      title: t('common.error'),
      message: err,
      type: 'error',
      duration: 5000,
    })
  }
})

/* ------------------------------------------------------------------ */
/*  Auto-refresh (QueueMonitor pattern): poll runtime health every     */
/*  30s. The config echo is NOT polled — it resyncs on mutations, and  */
/*  polling it would fight in-flight dialog edits.                     */
/* ------------------------------------------------------------------ */

let refreshTimer: ReturnType<typeof setInterval> | null = null
const isPolling = ref(false)

function startAutoRefresh() {
  stopAutoRefresh()
  refreshTimer = setInterval(async () => {
    if (isPolling.value) return
    isPolling.value = true
    try {
      await llm.fetch()
      checkLlmConfigured()
    } finally {
      isPolling.value = false
    }
  }, 30_000)
}

function stopAutoRefresh() {
  if (refreshTimer) {
    clearInterval(refreshTimer)
    refreshTimer = null
  }
}

/* ------------------------------------------------------------------ */
/*  Lifecycle                                                         */
/* ------------------------------------------------------------------ */

onMounted(() => {
  llm.fetch()
  loadProviderCards()
  checkLlmConfigured()
  startAutoRefresh()
})

onUnmounted(() => {
  stopAutoRefresh()
})
</script>

<template>
  <div class="llm-page">
    <!-- Page Header -->
    <div class="page-header">
      <div class="header-text">
        <h2 class="page-title">{{ $t('llm.title') }}</h2>
        <p class="page-subtitle">{{ $t('llm.subtitle') }}</p>
      </div>
      <el-button
        type="primary"
        :icon="Plus"
        :disabled="cardsLoading"
        @click="openAddDialog"
      >
        {{ $t('config.providerCards.add') }}
      </el-button>
    </div>

    <!-- Summary Stats -->
    <div class="stats-row">
      <el-card shadow="never" class="stat-card">
        <div class="stat-content">
          <el-icon class="stat-icon" :size="24"><Cpu /></el-icon>
          <div class="stat-body">
            <div class="stat-value">{{ providers.length }}</div>
            <div class="stat-label">{{ $t('llm.stats.providers') }}</div>
          </div>
        </div>
      </el-card>
      <el-card shadow="never" class="stat-card">
        <div class="stat-content">
          <el-icon class="stat-icon" :size="24" color="var(--success)"><CircleCheck /></el-icon>
          <div class="stat-body">
            <div class="stat-value" style="color: var(--success)">{{ healthyCount }}</div>
            <div class="stat-label">{{ $t('llm.status.healthy') }}</div>
          </div>
        </div>
      </el-card>
      <el-card shadow="never" class="stat-card">
        <div class="stat-content">
          <el-icon class="stat-icon" :size="24" color="var(--warning)"><Warning /></el-icon>
          <div class="stat-body">
            <div class="stat-value" style="color: var(--warning)">{{ degradedCount }}</div>
            <div class="stat-label">{{ $t('llm.status.degraded') }}</div>
          </div>
        </div>
      </el-card>
      <el-card shadow="never" class="stat-card">
        <div class="stat-content">
          <el-icon class="stat-icon" :size="24" color="var(--error)"><CircleClose /></el-icon>
          <div class="stat-body">
            <div class="stat-value" style="color: var(--error)">{{ errorCount }}</div>
            <div class="stat-label">{{ $t('llm.status.error') }}</div>
          </div>
        </div>
      </el-card>
      <el-card shadow="never" class="stat-card">
        <div class="stat-content">
          <el-icon class="stat-icon" :size="24" color="var(--offline)"><Remove /></el-icon>
          <div class="stat-body">
            <div class="stat-value" style="color: var(--offline)">{{ offlineCount }}</div>
            <div class="stat-label">{{ $t('llm.status.offline') }}</div>
          </div>
        </div>
      </el-card>
      <el-card shadow="never" class="stat-card">
        <div class="stat-content">
          <el-icon class="stat-icon" :size="24"><RefreshRight /></el-icon>
          <div class="stat-body">
            <div class="stat-value">{{ avgLatency }} ms</div>
            <div class="stat-label">{{ $t('llm.stats.avgLatency') }}</div>
          </div>
        </div>
      </el-card>
      <el-card shadow="never" class="stat-card">
        <div class="stat-content">
          <el-icon class="stat-icon" :size="24"><Cpu /></el-icon>
          <div class="stat-body">
            <div class="stat-value">{{ new Intl.NumberFormat('en-US').format(totalRequests) }}</div>
            <div class="stat-label">{{ $t('llm.stats.totalRequests') }}</div>
          </div>
        </div>
      </el-card>
    </div>

    <!-- LLM-not-configured banner: reviews cannot run without a usable LLM -->
    <el-alert
      v-if="llmNotConfigured"
      type="warning"
      :closable="false"
      :title="$t('config.llmNotConfiguredBanner')"
      class="llm-banner"
    />

    <!-- Loading Skeleton -->
    <div v-if="cardsLoading && providerCards.length === 0" class="skeleton-grid">
      <el-skeleton v-for="i in 2" :key="i" animated :rows="3" class="skeleton-card" />
    </div>

    <!-- Empty State -->
    <el-empty
      v-else-if="providerCards.length === 0"
      :description="$t('config.providerCards.empty')"
    >
      <el-button type="primary" :icon="Plus" @click="openAddDialog">
        {{ $t('config.providerCards.add') }}
      </el-button>
    </el-empty>

    <!-- Unified Provider Card Grid: config echo joined with runtime health,
         every mutation saves immediately (no page-level edit mode). -->
    <div v-else class="provider-grid">
      <ProviderConfigCard
        v-for="card in providerCards"
        :key="card.provider"
        :card="card"
        :primary="card.provider === primaryName"
        :health="healthByName.get(card.provider)"
        :testing="isCardTesting(card)"
        :saving="cardsSaving"
        @test="handleCardTest(card)"
        @edit="openEditDialog(card)"
        @delete="deleteCard(card)"
        @set-primary="setPrimary(card)"
      />
    </div>

    <ProviderEditDialog
      v-model:visible="dialogVisible"
      :mode="dialogMode"
      :initial="editingCard"
      :existing-names="providerCards.map((c) => c.provider)"
      :saving="cardsSaving"
      @save="handleDialogSave"
    />
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

/* LLM-not-configured banner sits below the stats row, above the cards */
.llm-banner {
  margin-bottom: 20px;
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
