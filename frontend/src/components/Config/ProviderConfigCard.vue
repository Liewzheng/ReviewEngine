<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { Connection, Edit, Delete, Star } from '@element-plus/icons-vue'
import type { LlmProvider, LlmProviderStatus } from '../../types/llm'
import { providerDisplayName, type ProviderCardState } from '../../composables/llmPayload'

const props = defineProps<{
  /** The provider card model (echo-shaped; the key is the masked sentinel). */
  card: ProviderCardState
  /** True for the primary provider card. */
  primary: boolean
  /** Runtime health entry (GET /llm/providers) matching this card, when the
   *  provider is active server-side. */
  health?: LlmProvider
  /** True while this provider's connectivity test is running. */
  testing?: boolean
  /** True while a mutation save is in flight (actions disabled). */
  saving?: boolean
}>()

const emit = defineEmits<{
  (e: 'test'): void
  (e: 'edit'): void
  (e: 'delete'): void
  (e: 'set-primary'): void
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

const formattedRequestsDisplay = computed(() => {
  const h = props.health
  if (!h || !hasLiveMetrics.value) return '—'
  return new Intl.NumberFormat('en-US').format(h.requestCount)
})

const errorRateColor = computed(() => {
  const h = props.health
  if (!h) return ''
  // When status is error, force red regardless of error rate value
  if (h.status === 'error') return 'var(--error)'
  if (h.errorRate < 0.01) return 'var(--success)'
  if (h.errorRate <= 0.05) return 'var(--warning)'
  return 'var(--error)'
})

const formattedErrorRateDisplay = computed(() => {
  const h = props.health
  if (!h || !hasLiveMetrics.value) return '—'
  return `${(h.errorRate * 100).toFixed(1)}%`
})

const usagePercent = computed(() => props.health?.usagePercent ?? 0)

const showUsage = computed(() => {
  const h = props.health
  return !!h && h.usagePercent !== undefined && h.configured
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

    <!-- Metrics Row (runtime health; '—' when unconfigured or offline) -->
    <div class="metrics-row">
      <div class="metric">
        <div class="metric-label">{{ $t('llm.metrics.latency') }}</div>
        <div class="metric-value" :style="latencyStyle">
          {{ formattedLatency }}
        </div>
      </div>
      <div class="metric">
        <div class="metric-label">{{ $t('llm.metrics.requests') }}</div>
        <div class="metric-value">{{ formattedRequestsDisplay }}</div>
      </div>
      <div class="metric">
        <div class="metric-label">{{ $t('llm.metrics.errors') }}</div>
        <div
          class="metric-value"
          :style="{
            color: formattedErrorRateDisplay !== '—' ? errorRateColor : undefined,
          }"
        >
          {{ formattedErrorRateDisplay }}
        </div>
      </div>
    </div>

    <!-- Usage Bar -->
    <div v-if="showUsage" class="usage-bar">
      <el-progress
        :percentage="usagePercent"
        :stroke-width="6"
        :color="'var(--brand)'"
        :show-text="false"
      />
      <span class="usage-label">{{ $t('llm.usage', { percent: usagePercent }) }}</span>
    </div>

    <!-- Last checked -->
    <div v-if="health" class="last-checked">
      {{ $t('llm.lastChecked', { date: new Date(health.lastChecked).toLocaleString() }) }}
    </div>

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
  margin-bottom: 12px;
}

.usage-label {
  display: block;
  font-size: 12px;
  color: var(--text-secondary);
  margin-top: 4px;
}

.last-checked {
  font-size: 11px;
  color: var(--text-secondary);
  margin-bottom: 12px;
  text-align: right;
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
