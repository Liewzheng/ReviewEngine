<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { Connection, Edit, Delete, Star } from '@element-plus/icons-vue'
import TestResultLine from '../common/TestResultLine.vue'
import type { LlmProvider, LlmProviderStatus, TestResult } from '../../types/llm'
import type { TransientResult } from '../../composables/useTransientResult'
import { providerDisplayName, type ProviderCardState } from '../../composables/llmPayload'

const props = defineProps<{
  /** The provider card model (echo-shaped; the key is the masked sentinel). */
  card: ProviderCardState
  /** True for the primary provider card. */
  primary: boolean
  /** Runtime health entry (GET /llm/providers) matching this card, when the
   *  provider is active server-side. */
  health?: LlmProvider
  /** RENG-55: 1-based rank of this provider in the runtime chain (the chain
   *  head is 1); `undefined` when the runtime provider list is unavailable. */
  chainPosition?: number
  /** True while this provider's connectivity test is running. */
  testing?: boolean
  /** True while a mutation save is in flight (actions disabled). */
  saving?: boolean
  /**
   * RENG-54: outcome of the last manual Test Connection, held as page-scoped
   * session state (see `useTransientResult`) — the health metrics below stay
   * the server's values, so the two never overwrite each other.
   */
  testResult?: TransientResult<TestResult> | null
  /**
   * RENG-56: length of the usage window the recorded numbers cover (days),
   * from the payload that carries them. `null`/absent = the server had no
   * usage history, in which case the usage row is not rendered at all.
   */
  usageWindowDays?: number | null
}>()

const emit = defineEmits<{
  (e: 'test'): void
  (e: 'edit'): void
  (e: 'delete'): void
  (e: 'set-primary'): void
  /** Dismiss the recorded test result. */
  (e: 'clear-test'): void
}>()

const { t } = useI18n()

const statusConfig: Record<
  LlmProviderStatus,
  { labelKey: string; type: 'success' | 'warning' | 'danger' | 'info' }
> = {
  healthy: { labelKey: 'llm.status.healthy', type: 'success' },
  degraded: { labelKey: 'llm.status.degraded', type: 'warning' },
  error: { labelKey: 'llm.status.error', type: 'danger' },
  offline: { labelKey: 'llm.status.offline', type: 'info' },
}

const displayName = computed(() => providerDisplayName(props.card.provider))
const avatarLetter = computed(() => displayName.value.charAt(0).toUpperCase() || '?')

const healthInfo = computed(() => {
  if (!props.health) return null
  const c = statusConfig[props.health.status]
  return { ...c, label: t(c.labelKey) }
})

/** Masked key indicator: the echo carries `***` when a key is stored. */
const keyIndicator = computed(() => (props.card.apiKey ? '●●●●●' : t('config.notSet')))

/* ------------------------------------------------------------------ */
/*  Runtime health metrics (joined by provider name) — plain reactive  */
/*  values, no count-up animation or status-change flash.              */
/* ------------------------------------------------------------------ */

/** True when the health entry carries live metrics worth showing. */
const hasLiveMetrics = computed(() => {
  const h = props.health
  return !!h && h.configured && h.status !== 'offline'
})

const formattedLatency = computed(() => {
  const h = props.health
  if (!h || !hasLiveMetrics.value) return '—'
  return `${h.latencyMs} ms`
})

const latencyColor = computed(() => {
  const h = props.health
  if (!h || !hasLiveMetrics.value || h.latencyMs === 0) return ''
  if (h.latencyMs < 500) return 'var(--success)'
  if (h.latencyMs <= 1500) return 'var(--warning)'
  return 'var(--error)'
})

const latencyStyle = computed(() => {
  if (formattedLatency.value === '—') return {}
  return { color: latencyColor.value }
})

/* ------------------------------------------------------------------ */
/*  Recorded usage (RENG-56)                                           */
/*                                                                     */
/*  These numbers come from the reviews that actually ran on this      */
/*  provider (`reviews.llm_summary`), aggregated over the window the    */
/*  payload reports. They are independent of the probe: a provider     */
/*  that is offline right now still has last week's usage. `null`      */
/*  means the server has no number for it — the card then shows `—`,   */
/*  never a stand-in 0.                                                */
/* ------------------------------------------------------------------ */

/** True when the server could read usage history for this payload. */
const hasUsageWindow = computed(
  () => props.usageWindowDays !== null && props.usageWindowDays !== undefined
)

const formattedRequestsDisplay = computed(() => {
  const count = props.health?.requestCount
  if (count === null || count === undefined) return '—'
  return new Intl.NumberFormat('en-US').format(count)
})

const formattedSuccessRateDisplay = computed(() => {
  const rate = props.health?.successRate
  if (rate === null || rate === undefined) return '—'
  return `${(rate * 100).toFixed(1)}%`
})

const successRateColor = computed(() => {
  const h = props.health
  if (!h || h.successRate === null || h.successRate === undefined) return ''
  if (h.status === 'error') return 'var(--error)'
  if (h.successRate >= 0.99) return 'var(--success)'
  if (h.successRate >= 0.95) return 'var(--warning)'
  return 'var(--error)'
})

/** Share of the window's usage as a 0–100 percentage, or null when unknown. */
const usageSharePercent = computed(() => {
  const share = props.health?.usageShare
  if (share === null || share === undefined) return null
  return Math.round(share * 1000) / 10
})

const usageShareLabel = computed(() => {
  if (usageSharePercent.value === null) return '—'
  return t('llm.usageShare', {
    percent: usageSharePercent.value,
    days: props.usageWindowDays,
  })
})

const lastUsedDisplay = computed(() => {
  const at = props.health?.lastUsedAt
  if (!at) return '—'
  const d = new Date(at)
  return Number.isNaN(d.getTime()) ? '—' : d.toLocaleString()
})

const lastCheckedDisplay = computed(() => {
  const at = props.health?.lastChecked
  if (!at) return '—'
  const d = new Date(at)
  return Number.isNaN(d.getTime()) ? '—' : d.toLocaleString()
})

/* ------------------------------------------------------------------ */
/*  Last manual test (RENG-54) — session state, not polled data.       */
/* ------------------------------------------------------------------ */

/** Outcome text for the recorded test result, or '' when none was recorded. */
const testResultText = computed(() => {
  const result = props.testResult
  if (!result) return ''
  return result.value.success
    ? t('config.llm.connected', { n: result.value.latencyMs ?? 0 })
    : t('config.llm.testFailed', { error: result.value.error ?? t('errors.unknown') })
})
</script>

<template>
  <el-card shadow="hover" :body-style="{ padding: '20px' }" class="provider-config-card">
    <!-- Header Row -->
    <div class="card-header">
      <div class="provider-info">
        <span class="provider-avatar" aria-hidden="true">{{ avatarLetter }}</span>
        <span class="provider-name" :title="card.provider">{{ displayName }}</span>
        <el-tag v-if="primary" type="warning" effect="dark" size="small" class="primary-badge">
          {{ $t('config.providerCards.primaryBadge') }}
        </el-tag>
        <!-- RENG-55: quiet chain-order marker — #1 is the provider a review
             starts on, the rest are the fallback order behind it. -->
        <el-tag v-if="chainPosition" type="info" effect="plain" size="small" class="chain-badge">
          {{ $t('config.providerCards.chainPosition', { n: chainPosition }) }}
        </el-tag>
      </div>
      <el-tag
        v-if="healthInfo"
        :type="healthInfo.type"
        effect="dark"
        size="small"
        class="status-badge"
        :class="{ 'offline-badge': health?.status === 'offline' }"
      >
        {{ healthInfo.label }}
      </el-tag>
    </div>

    <!-- Config Rows -->
    <div class="config-rows">
      <div class="config-row">
        <span class="row-label">{{ $t('config.providers.apiBaseUrl') }}</span>
        <span class="row-value mono" :title="card.apiBaseUrl">{{ card.apiBaseUrl || '—' }}</span>
      </div>
      <div class="config-row">
        <span class="row-label">{{ $t('config.providers.defaultModel') }}</span>
        <span class="row-value mono" :title="card.defaultModel">{{ card.defaultModel || '—' }}</span>
      </div>
      <div class="config-row">
        <span class="row-label">{{ $t('config.providers.apiKey') }}</span>
        <span class="row-value mono">{{ keyIndicator }}</span>
      </div>
    </div>

    <!-- Metrics Row: latency = the live probe (RENG-36); requests and
         success rate = recorded usage of the window (RENG-56). '—' whenever
         the server has no number, never a stand-in 0. -->
    <div class="metrics-row">
      <div class="metric">
        <div class="metric-label">{{ $t('llm.metrics.latency') }}</div>
        <div class="metric-value" :style="latencyStyle">
          {{ formattedLatency }}
        </div>
      </div>
      <div class="metric">
        <div class="metric-label" :title="hasUsageWindow ? $t('llm.usageWindow', { days: usageWindowDays }) : undefined">
          {{ $t('llm.metrics.requests') }}
        </div>
        <div class="metric-value">{{ formattedRequestsDisplay }}</div>
      </div>
      <div class="metric">
        <div class="metric-label">{{ $t('llm.metrics.successRate') }}</div>
        <div
          class="metric-value"
          :style="{
            color: formattedSuccessRateDisplay !== '—' ? successRateColor : undefined,
          }"
        >
          {{ formattedSuccessRateDisplay }}
        </div>
      </div>
    </div>

    <!-- Usage share over the window (RENG-56) — this provider's slice of all
         recorded usage. Rendered only when the server could read the window
         AND holds usage to divide by; otherwise there is no bar to draw. -->
    <div v-if="hasUsageWindow && usageSharePercent !== null" class="usage-bar">
      <el-progress
        :percentage="usageSharePercent"
        :stroke-width="6"
        :color="'var(--brand)'"
        :show-text="false"
      />
      <span class="usage-label">{{ usageShareLabel }}</span>
    </div>

    <!-- Last used / last checked: both real timestamps, '—' when unknown -->
    <div v-if="hasUsageWindow" class="usage-meta">
      {{ $t('llm.lastUsed', { date: lastUsedDisplay }) }}
    </div>
    <div v-if="health" class="last-checked">
      {{ $t('llm.lastChecked', { date: lastCheckedDisplay }) }}
    </div>

    <!-- Last manual test (RENG-54): kept as page-session state so the next
         poll tick (30s) cannot wipe the result the user just asked for. -->
    <TestResultLine
      v-if="testResult && testResultText"
      class="last-test"
      :type="testResult.value.success ? 'success' : 'danger'"
      :text="testResultText"
      :at="testResult.at"
      @dismiss="emit('clear-test')"
    />

    <!-- Action Row -->
    <div class="action-row">
      <el-button
        size="small"
        :icon="Connection"
        :loading="testing"
        :disabled="!health || saving"
        :title="!health ? $t('config.providerCards.testUnavailable') : undefined"
        @click="emit('test')"
      >
        {{ $t('common.testConnection') }}
      </el-button>
      <el-button size="small" :icon="Edit" :disabled="saving" @click="emit('edit')">
        {{ $t('common.edit') }}
      </el-button>
      <el-button
        v-if="!primary"
        size="small"
        :icon="Star"
        :disabled="saving"
        @click="emit('set-primary')"
      >
        {{ $t('config.providerCards.setPrimary') }}
      </el-button>
      <el-button
        size="small"
        text
        type="danger"
        :icon="Delete"
        :disabled="saving"
        class="delete-btn"
        @click="emit('delete')"
      />
    </div>
  </el-card>
</template>

<style scoped>
.provider-config-card {
  background-color: var(--bg-card);
  border: 1px solid var(--border-color);
  border-radius: var(--radius-md);
  box-shadow: var(--shadow-card);
  transition: border-color 0.2s ease, box-shadow 0.2s ease, transform 0.2s ease;
}

.provider-config-card:hover {
  border-color: var(--brand);
  box-shadow: 0 0 0 1px var(--brand), var(--shadow-card);
  transform: translateY(-2px);
}

.card-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
  margin-bottom: 16px;
}

.provider-info {
  display: flex;
  align-items: center;
  gap: 10px;
  min-width: 0;
}

.provider-avatar {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 32px;
  height: 32px;
  border-radius: 50%;
  background: var(--bg-active);
  color: var(--brand);
  font-size: 15px;
  font-weight: 600;
  flex-shrink: 0;
  user-select: none;
}

.provider-name {
  font-size: 16px;
  font-weight: 600;
  color: var(--text-primary);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.primary-badge {
  flex-shrink: 0;
}

/* RENG-55: quiet chain marker so the header still reads as name-first. */
.chain-badge {
  flex-shrink: 0;
  font-family: var(--font-mono);
  font-size: 11px;
  opacity: 0.75;
}

.status-badge {
  flex-shrink: 0;
}

.status-badge.offline-badge {
  background-color: var(--offline) !important;
  border-color: var(--offline) !important;
  color: #fff !important;
}

.config-rows {
  display: flex;
  flex-direction: column;
  gap: 8px;
  margin-bottom: 16px;
}

.config-row {
  display: flex;
  align-items: baseline;
  gap: 10px;
  min-width: 0;
}

.row-label {
  font-size: 11px;
  color: var(--text-secondary);
  text-transform: uppercase;
  letter-spacing: 0.05em;
  flex-shrink: 0;
  width: 96px;
}

.row-value {
  font-size: 13px;
  color: var(--text-primary);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.row-value.mono {
  font-family: var(--font-mono);
  font-size: 12px;
}

.metrics-row {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 8px;
  margin-bottom: 16px;
}

.metric {
  text-align: center;
}

.metric-label {
  font-size: 11px;
  color: var(--text-secondary);
  text-transform: uppercase;
  letter-spacing: 0.05em;
  margin-bottom: 4px;
}

.metric-value {
  font-family: var(--font-mono);
  font-size: 18px;
  font-weight: 500;
  color: var(--text-primary);
  transition: color 0.2s ease;
}

.usage-bar {
  margin-bottom: 8px;
}

.usage-label {
  display: block;
  font-size: 12px;
  color: var(--text-secondary);
  margin-top: 4px;
}

/* RENG-56: the newest recorded use, next to the "last checked" probe time */
.usage-meta {
  font-size: 11px;
  color: var(--text-secondary);
  margin-bottom: 4px;
  text-align: right;
}

.last-checked {
  font-size: 11px;
  color: var(--text-secondary);
  margin-bottom: 12px;
  text-align: right;
}

/* RENG-54: the last manual test outcome sits above the actions, not in the
   metrics row — the metrics stay the server's numbers. */
.last-test {
  margin-bottom: 12px;
}

.action-row {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
  padding-top: 12px;
  border-top: 1px solid var(--border-color);
}

.action-row .el-button {
  margin-left: 0;
}

.delete-btn {
  margin-left: auto;
}
</style>
