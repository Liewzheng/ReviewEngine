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
