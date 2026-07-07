<template>
  <span class="status-badge" :class="[`status-${status}`, size]">
    <span class="status-dot" :class="[`status-${status}`]"></span>
    <span v-if="showText" class="status-text">{{ displayText }}</span>
  </span>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import type { HealthState, ReviewStatus } from '../../types/dashboard'

interface Props {
  status: HealthState | ReviewStatus
  showText?: boolean
  size?: 'small' | 'medium' | 'large'
}

const props = withDefaults(defineProps<Props>(), {
  showText: true,
  size: 'medium',
})

const statusTextMap: Record<string, string> = {
  success: 'Operational',
  warning: 'Degraded',
  error: 'Error',
  offline: 'Offline',
  running: 'In Progress',
  queued: 'Queued',
  failed: 'Failed',
}

const displayText = computed(() => statusTextMap[props.status] || props.status)
</script>

<style scoped>
.status-badge {
  display: inline-flex;
  align-items: center;
  gap: 6px;
}

.status-dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  flex-shrink: 0;
}

.status-dot.status-success {
  background: var(--success);
  box-shadow: 0 0 0 2px rgba(34, 197, 94, 0.2);
}
.status-dot.status-warning {
  background: var(--warning);
  box-shadow: 0 0 0 2px rgba(245, 158, 11, 0.2);
}
.status-dot.status-error {
  background: var(--error);
  box-shadow: 0 0 0 2px rgba(239, 68, 68, 0.2);
}
.status-dot.status-offline {
  background: var(--offline);
  box-shadow: 0 0 0 2px rgba(107, 114, 128, 0.2);
}
.status-dot.status-running {
  background: var(--info);
  box-shadow: 0 0 0 2px rgba(59, 130, 246, 0.2);
  animation: pulse-dot 2s infinite;
}
.status-dot.status-queued {
  background: var(--text-secondary);
  box-shadow: 0 0 0 2px rgba(148, 163, 184, 0.2);
}
.status-dot.status-failed {
  background: var(--error);
  box-shadow: 0 0 0 2px rgba(239, 68, 68, 0.2);
}

.status-text {
  font-size: 13px;
  font-weight: 500;
  color: var(--text-primary);
}

.status-badge.small .status-dot {
  width: 6px;
  height: 6px;
}
.status-badge.small .status-text {
  font-size: 12px;
}

.status-badge.large .status-dot {
  width: 10px;
  height: 10px;
}

@keyframes pulse-dot {
  0%, 100% { opacity: 1; }
  50% { opacity: 0.5; }
}
</style>
