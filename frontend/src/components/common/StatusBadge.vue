<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { Loading } from '@element-plus/icons-vue'

type BadgeStatus = 'success' | 'warning' | 'error' | 'info' | 'offline' | 'running' | 'queued' | 'failed' | 'completed' | 'cancelled' | 'skipped'

interface Props {
  status: BadgeStatus
  size?: 'small' | 'default'
  dotOnly?: boolean
  showText?: boolean
}

const props = withDefaults(defineProps<Props>(), {
  size: 'small',
  dotOnly: false,
  showText: true,
})

const { t } = useI18n()

const statusConfigMap: Record<string, { type: string; textKey: string; color: string; effect?: string }> = {
  success:    { type: 'success', textKey: 'common.status.operational', color: 'var(--success)' },
  running:    { type: 'success', textKey: 'common.status.inProgress',  color: 'var(--success)' },
  completed:  { type: 'success', textKey: 'common.status.completed',   color: 'var(--success)' },
  warning:    { type: 'warning', textKey: 'common.status.degraded',    color: 'var(--warning)' },
  queued:     { type: 'info',    textKey: 'common.status.queued',      color: 'var(--info)' },
  info:       { type: 'info',    textKey: 'common.status.info',        color: 'var(--info)' },
  error:      { type: 'danger',  textKey: 'common.status.error',       color: 'var(--error)' },
  failed:     { type: 'danger',  textKey: 'common.status.failed',      color: 'var(--error)' },
  offline:    { type: 'info',    textKey: 'common.status.offline',     color: 'var(--offline)' },
  cancelled:  { type: 'info',    textKey: 'common.status.cancelled',   color: 'var(--text-secondary)', effect: 'plain' },
  skipped:    { type: 'info',    textKey: 'common.status.skipped',     color: 'var(--text-secondary)', effect: 'plain' },
}

const config = computed(() => {
  const c = statusConfigMap[props.status] || { type: 'info', textKey: '', color: 'var(--text-secondary)' }
  return { ...c, text: c.textKey ? t(c.textKey) : (props.status as string) }
})

const dotColor = computed(() => config.value.color)
const isRunning = computed(() => props.status === 'running')
</script>

<template>
  <!-- dotOnly variant -->
  <span
    v-if="dotOnly"
    class="status-badge-dot-only"
    :class="{ 'is-running': isRunning }"
    :style="{ backgroundColor: dotColor }"
    aria-hidden="true"
  />

  <!-- ElTag variant (ReviewHistory style) -->
  <el-tag
    v-else-if="!showText && !dotOnly"
    :type="config.type as any"
    :effect="config.effect || 'light'"
    :size="size"
    class="status-badge-tag"
  >
    <el-icon v-if="status === 'running'" class="is-loading"><Loading /></el-icon>
    {{ config.text }}
  </el-tag>

  <!-- Custom badge variant (Dashboard style with text) -->
  <span
    v-else
    class="status-badge"
    :class="[`size-${size}`, `status-${status}`]"
  >
    <span
      class="status-badge-dot"
      :class="{ 'is-running': isRunning }"
      :style="{ backgroundColor: dotColor }"
      aria-hidden="true"
    />
    <span v-if="showText" class="status-badge-text">{{ config.text }}</span>
  </span>
</template>

<style scoped>
.status-badge {
  display: inline-flex;
  align-items: center;
  gap: 6px;
}

.status-badge-dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  flex-shrink: 0;
}

.status-badge-dot.is-running {
  animation: pulse 2s infinite;
}

.status-badge-text {
  font-size: 12px;
  font-weight: 500;
  color: var(--text-primary);
}

.status-badge.size-small .status-badge-dot {
  width: 6px;
  height: 6px;
}
.status-badge.size-small .status-badge-text {
  font-size: 11px;
}

.status-badge.size-default .status-badge-dot {
  width: 8px;
  height: 8px;
}
.status-badge.size-default .status-badge-text {
  font-size: 13px;
}

.status-badge-dot-only {
  display: inline-block;
  width: 8px;
  height: 8px;
  border-radius: 50%;
  flex-shrink: 0;
}
.status-badge-dot-only.is-running {
  animation: pulse 2s infinite;
}

.status-badge-tag {
  display: inline-flex;
  align-items: center;
  gap: 4px;
}

.is-loading {
  animation: rotating 2s linear infinite;
}

@keyframes pulse {
  0%, 100% { opacity: 1; }
  50% { opacity: 0.4; }
}

@keyframes rotating {
  from { transform: rotate(0deg); }
  to { transform: rotate(360deg); }
}
</style>
