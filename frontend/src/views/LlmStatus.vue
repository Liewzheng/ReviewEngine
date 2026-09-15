<script setup lang="ts">
import { ref, computed, watch, onMounted, onUnmounted } from 'vue'
import { ElMessage, ElNotification } from 'element-plus'
import { useI18n } from 'vue-i18n'
import { RefreshRight, Cpu, CircleCheck, Warning, CircleClose, Remove, Plus } from '@element-plus/icons-vue'
import { useLlmStatus } from '../composables/useLlmStatus'
import { useProviderCards } from '../composables/useProviderCards'
import { useAutoRefresh } from '../composables/useAutoRefresh'
import { useRecentLlmUsage } from '../composables/useRecentLlmUsage'
import type { ProviderCardState } from '../composables/llmPayload'
import { getSystemHealth } from '../services/health'
import PageHeader from '../components/common/PageHeader.vue'
import ProviderCardCompact from '../components/Config/ProviderCardCompact.vue'
import ProviderEditDialog from '../components/Config/ProviderEditDialog.vue'
import {
  dialogForDuplicate,
  dialogForEdit,
  healthTriple,
  matchHealthToCards,
  matchProbeMessages,
} from '../components/Config/providerCardState'

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

/** RENG-57: length of the latency window the recorded averages cover (days);
 *  null when the server could not read the call samples, which turns the KPI
 *  into `—` rather than a fabricated number. */
const latencyWindowDays = computed(() => (llm.latencyAvailable.value ? llm.latencyWindowDays.value : null))

/** Mean recorded call latency over the window, weighted by each provider's
 *  successful sample count — i.e. the average call this instance made, not the
 *  average of the per-provider averages (a provider with 100 calls is not the
 *  same evidence as one with 2). `—` when no provider has a recorded sample,
 *  including when the samples could not be read at all; the probe's own
 *  instantaneous number is NOT a substitute for it. */
const avgLatency = computed(() => {
  const measured = providers.value.filter(
    (p) => typeof p.avgLatencyMs === 'number' && (p.latencySampleCount ?? 0) > 0
  )
  if (!measured.length) return null
  const total = measured.reduce((sum, p) => sum + (p.latencySampleCount ?? 0), 0)
  const weighted = measured.reduce((sum, p) => sum + (p.avgLatencyMs as number) * (p.latencySampleCount ?? 0), 0)
  return Math.round(weighted / total)
})

/** RENG-56: usage window the per-provider numbers cover; null when the
 *  server could not read the usage history, which hides the usage row. */
const usageWindowDays = computed(() => (llm.usageAvailable.value ? llm.usageWindowDays.value : null))

/** RENG-56: usages recorded in the window across ALL provider names — what
 *  the cards' shares are taken against; null when the server has no history. */
const totalRequests = computed(() => (llm.usageAvailable.value ? llm.usageTotal.value : null))

const totalRequestsDisplay = computed(() =>
  totalRequests.value === null ? '—' : new Intl.NumberFormat('en-US').format(totalRequests.value)
)

/* ------------------------------------------------------------------ */
/*  LLM Configuration — unified provider cards with immediate saves   */
/* ------------------------------------------------------------------ */

/** True when the server reports no usable LLM via /system/health. */
const llmNotConfigured = ref(false)

/** Probe failure text per card, from `/system/health`.
 *
 * `GET /llm/providers` reports the probe's status and timing but no message,
 * while `/system/health` carries the probe's own words ("HTTP 401
 * Unauthorized") — which is what an errored card has to show in its footer
 * slot. Kept in page state rather than merged into `providers` because the
 * 30 s poll reconciles that list in place. */
const probeMessages = ref<(string | null)[]>([])

/** Refresh the not-configured banner and the per-card probe messages.
 *  Fail-open: a health-check error keeps the current values. */
async function checkLlmConfigured() {
  if (import.meta.env.VITE_USE_LLM_MOCKS === 'true') {
    const { MOCK_HEALTH_ROWS } = await import('../dev-mocks/llm-providers.mock')
    probeMessages.value = matchProbeMessages(providerCards.value, MOCK_HEALTH_ROWS)
    llmNotConfigured.value = false
    return
  }
  getSystemHealth()
    .then((health) => {
      llmNotConfigured.value = health.llmConfigured === false
      probeMessages.value = matchProbeMessages(providerCards.value, health.llmProviders ?? [])
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
  toggleDisabled,
  reorder,
  deleteCard,
} = useProviderCards({
  statusProviders: llm.providers,
  afterSave: () => {
    // Reflect the new config in the health cards and the banner.
    llm.fetch()
    checkLlmConfigured()
  },
})

/** Runtime health entries aligned with the config cards (RENG-75).
 *
 * The join is the visible `(provider, base, model)` triple, never the name:
 * the name is a display label two cards may share, and joining on it would
 * show the first account's latency on the second card. */
const cardHealth = computed(() => matchHealthToCards(providerCards.value, llm.providers.value))

/* ------------------------------------------------------------------ */
/*  Recent usage (RENG-55): what the newest reviews actually ran on    */
/* ------------------------------------------------------------------ */

/**
 * The chain head per the runtime payload (`GET /llm/providers`), falling back
 * to the config echo's primary so the marker works even before the health
 * list arrives. `''` when nothing is configured.
 */
const chainHeadName = computed(
  () => providers.value.find((p) => p.isPrimary)?.name ?? primaryName.value ?? ''
)

const {
  entries: recentUsage,
  loading: recentUsageLoading,
  load: loadRecentUsage,
} = useRecentLlmUsage()

/** Local timestamp for a usage row (`—` for an unparsable value). */
function formatUsageWhen(createdAt: string): string {
  const d = new Date(createdAt)
  return Number.isNaN(d.getTime()) ? '—' : d.toLocaleString()
}

// --- Add/Edit dialog ---
const dialogVisible = ref(false)
const dialogMode = ref<'add' | 'edit'>('add')
const editingCard = ref<ProviderCardState | null>(null)
/** Grid index the dialog edits (edit mode) — a card is addressed by position,
 *  never by name (RENG-75: two cards may share a name). */
const editingIndex = ref(-1)

function openAddDialog() {
  dialogMode.value = 'add'
  editingCard.value = null
  editingIndex.value = -1
  dialogVisible.value = true
}

function openEditDialog(index: number) {
  const open = dialogForEdit(providerCards.value, index)
  if (!open) return
  dialogMode.value = open.mode
  editingCard.value = open.initial
  editingIndex.value = open.index
  dialogVisible.value = true
}

/**
 * "Duplicate card": the add dialog, pre-filled with the card the user picked
 * and its API key. The key is the `***` sentinel the echo carries (the secret
 * never leaves the server) — the masked-keep rule resolves it back to the
 * original entry's stored key while the triple still matches, so the copy
 * starts as a real sibling holding the same credential. Editing the copy's
 * base URL or model is what makes it a distinct entry, and that edit needs a
 * re-entered key (the same rule the API applies to any masked key).
 */
function openDuplicateDialog(index: number) {
  const open = dialogForDuplicate(providerCards.value, index)
  if (!open) return
  dialogMode.value = open.mode
  editingCard.value = open.initial
  editingIndex.value = open.index
  dialogVisible.value = true
}

function onToggleDisabled(index: number) {
  void toggleDisabled(index)
}

function onReorder(fromIndex: number, toIndex: number) {
  void reorder(fromIndex, toIndex)
}

function onDeleteCard(index: number) {
  void deleteCard(index)
}

async function handleDialogSave(form: ProviderCardState) {
  const isEdit = dialogMode.value === 'edit'
  const previousHealth = isEdit ? cardHealth.value[editingIndex.value] : undefined
  const ok = isEdit ? await editCard(editingIndex.value, form) : await addCard(form)
  if (ok) {
    // The configuration that was tested just changed, so the recorded
    // result no longer describes this provider (RENG-54).
    if (previousHealth) llm.testResults.clear(healthTriple(previousHealth))
    dialogVisible.value = false
  }
}

/** Card-level connectivity test rides the server-side probe (stored key),
 *  so no secret ever round-trips through the browser. The outcome is kept as
 *  page-session state (RENG-54) — see `cardTestResult`. */
async function handleCardTest(index: number) {
  const health = cardHealth.value[index]
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

/** Last manual test result for a card's runtime entry. Keyed by the entry's
 *  triple — the identity the config echo can express too — so two cards that
 *  share a provider name never share a result. */
function cardTestResult(index: number) {
  const health = cardHealth.value[index]
  return health ? (llm.testResults.get(healthTriple(health))?.value ?? null) : null
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
/*  Auto-refresh: poll runtime health every 30s via the shared          */
/*  composable (pauses while hidden, immediate fetch on return). The    */
/*  tick is silent — it never flips `loading` and a failed poll keeps   */
/*  the last good provider list. The config echo is NOT polled — it     */
/*  resyncs on mutations, and polling it would fight in-flight dialog   */
/*  edits. The recent-usage strip rides the same tick (8 reviews, one   */
/*  list call) so a review served by a fallback provider shows up       */
/*  without a manual refresh.                                           */
/* ------------------------------------------------------------------ */

const llmAutoRefresh = useAutoRefresh(async () => {
  await llm.fetch(true)
  checkLlmConfigured()
  await loadRecentUsage(chainHeadName.value)
}, 30_000)

/* ------------------------------------------------------------------ */
/*  Lifecycle                                                         */
/* ------------------------------------------------------------------ */

onMounted(async () => {
  await llm.fetch()
  loadProviderCards()
  checkLlmConfigured()
  await loadRecentUsage(chainHeadName.value)
  llmAutoRefresh.start()
})

onUnmounted(() => {
  llmAutoRefresh.stop()
})
</script>

<template>
  <div class="llm-page">
    <PageHeader :title="$t('llm.title')" :subtitle="$t('llm.subtitle')">
      <template #actions>
        <el-button type="primary" :icon="Plus" :disabled="cardsLoading" @click="openAddDialog">
          {{ $t('config.providerCards.add') }}
        </el-button>
      </template>
    </PageHeader>

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
            <div class="stat-value" :class="{ 'is-empty': avgLatency === null }">
              {{ avgLatency === null ? '—' : `${avgLatency} ms` }}
            </div>
            <div class="stat-label">
              {{ latencyWindowDays === null
                ? $t('llm.stats.avgLatency')
                : $t('llm.stats.avgLatencyWindow', { days: latencyWindowDays }) }}
            </div>
          </div>
        </div>
      </el-card>
      <el-card shadow="never" class="stat-card">
        <div class="stat-content">
          <el-icon class="stat-icon" :size="24"><Cpu /></el-icon>
          <div class="stat-body">
            <div class="stat-value" :class="{ 'is-empty': totalRequests === null }">
              {{ totalRequestsDisplay }}
            </div>
            <div class="stat-label">
              {{ usageWindowDays === null
                ? $t('llm.stats.totalRequests')
                : $t('llm.stats.totalRequestsWindow', { days: usageWindowDays }) }}
            </div>
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

    <!-- Unified Provider Card Grid: config echo joined with runtime health by
         the visible (provider, base, model) triple, every mutation saved
         immediately (no page-level edit mode). No buttons on the card face —
         right-click (or click, for keyboard/touch) opens the action menu, and
         the header is the drag handle for reordering the chain. -->
    <div v-else class="provider-grid">
      <ProviderCardCompact
        v-for="(card, index) in providerCards"
        :key="`${card.provider}-${card.apiBaseUrl}-${card.defaultModel}-${index}`"
        :card="card"
        :health="cardHealth[index]"
        :probe-message="probeMessages[index] ?? null"
        :usage-window-days="usageWindowDays"
        :test-result="cardTestResult(index)"
        :saving="cardsSaving"
        :index="index"
        @edit="openEditDialog(index)"
        @duplicate="openDuplicateDialog(index)"
        @test="handleCardTest(index)"
        @toggle-disabled="onToggleDisabled(index)"
        @delete="onDeleteCard(index)"
        @reorder="onReorder"
      />
    </div>

    <!-- Recent usage (RENG-55): the provider/model the newest reviews ran on
         (`reviews.llm_summary`), so a review served by a fallback provider is
         visible instead of looking like a normal run. -->
    <el-card shadow="never" class="recent-usage-card">
      <div class="recent-usage-header">
        <span class="recent-usage-title">{{ $t('llm.recentUsage.title') }}</span>
        <span v-if="chainHeadName" class="recent-usage-hint">
          {{ $t('llm.recentUsage.chainHint', { name: chainHeadName }) }}
        </span>
      </div>
      <div v-if="recentUsageLoading && recentUsage.length === 0" class="recent-usage-loading">
        <el-skeleton :rows="2" animated />
      </div>
      <el-empty
        v-else-if="recentUsage.length === 0"
        :description="$t('llm.recentUsage.empty')"
        :image-size="60"
      />
      <ul v-else class="recent-usage-list">
        <li v-for="entry in recentUsage" :key="entry.id" class="recent-usage-item">
          <span class="recent-usage-time">{{ formatUsageWhen(entry.createdAt) }}</span>
          <span class="recent-usage-models">
            {{ entry.usages.map((u) => `${u.provider}/${u.model}`).join(' · ') }}
          </span>
          <el-tag v-if="entry.primaryUnused" type="warning" effect="plain" size="small">
            {{ $t('llm.recentUsage.primaryUnused') }}
          </el-tag>
        </li>
      </ul>
    </el-card>

    <ProviderEditDialog
      v-model:visible="dialogVisible"
      :mode="dialogMode"
      :initial="editingCard"
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
  font-size: 28px;
  font-weight: 600;
  color: var(--text-primary);
  line-height: 1.2;
  font-variant-numeric: tabular-nums;
}

.stat-value.is-empty {
  color: var(--text-tertiary);
}

.stat-label {
  font-size: 12px;
  color: var(--text-secondary);
  margin-top: 2px;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
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

/* Provider Grid: every card stretches to the row's height so the footer
   slots line up across the grid (RENG-76 R0.3). */
.provider-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(320px, 1fr));
  align-items: stretch;
  gap: 16px;
}

/* Recent usage (RENG-55) — same card chrome as the grid, quieter type. */
.recent-usage-card {
  margin-top: 24px;
  background-color: var(--bg-card);
  border: 1px solid var(--border-color);
  border-radius: var(--radius-md);
  box-shadow: var(--shadow-card);
}

.recent-usage-header {
  display: flex;
  align-items: baseline;
  gap: 12px;
  flex-wrap: wrap;
  margin-bottom: 12px;
}

.recent-usage-title {
  font-size: 14px;
  font-weight: 600;
  color: var(--text-primary);
}

.recent-usage-hint {
  font-size: 12px;
  color: var(--text-secondary);
}

.recent-usage-list {
  list-style: none;
  margin: 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.recent-usage-item {
  display: flex;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
  padding-top: 8px;
  border-top: 1px solid var(--border-color);
  font-size: 13px;
}

.recent-usage-item:first-child {
  border-top: none;
  padding-top: 0;
}

.recent-usage-time {
  font-size: 12px;
  color: var(--text-secondary);
  min-width: 150px;
}

.recent-usage-models {
  font-family: var(--font-mono);
  font-size: 12px;
  color: var(--text-primary);
}

/* Responsive */
@media (max-width: 768px) {
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
