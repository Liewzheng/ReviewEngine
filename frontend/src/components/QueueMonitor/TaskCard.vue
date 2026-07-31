<template>
  <el-card class="task-card" :class="{ 'is-paused': isPaused && task.status === 'queued', 'sse-update': wasUpdated }" shadow="never">
    <div class="task-header">
      <span
        role="img"
        :aria-label="task.status"
        class="status-dot"
        :class="{ 'is-running': task.status === 'running' }"
        :style="{ backgroundColor: statusColor }"
      ></span>
      <span class="task-title" :title="task.mrTitle">{{ task.mrTitle }}</span>
    </div>
    <div class="task-subtitle">{{ task.project }} / {{ task.repository }}</div>

    <el-progress
      v-if="task.progress != null"
      :percentage="task.progress"
      :color="statusColor"
      :stroke-width="6"
      :show-text="true"
      class="task-progress"
    />

    <div v-if="hasExpert || hasElapsed" class="task-meta">
      <span v-if="hasExpert">Expert: {{ task.expertName }}</span>
      <span v-if="hasExpert && hasElapsed" class="meta-sep">·</span>
      <span v-if="hasElapsed">{{ formattedElapsed }}</span>
    </div>

    <div v-if="task.errorMessage" class="task-error">
      {{ task.errorMessage }}
    </div>

    <div class="task-actions">
      <el-button-group size="small">
        <template v-if="task.status === 'running'">
          <el-button type="danger" plain @click="handleCancel">
            <el-icon><Close /></el-icon>
            <span>Cancel</span>
          </el-button>
          <el-button type="primary" plain @click="handleViewLogs">
            <el-icon><List /></el-icon>
            <span>Logs</span>
          </el-button>
        </template>
        <template v-else-if="task.status === 'queued'">
          <el-button type="danger" plain @click="handleCancel">
            <el-icon><Close /></el-icon>
            <span>Cancel</span>
          </el-button>
        </template>
        <template v-else-if="task.status === 'failed'">
          <el-button type="warning" plain @click="handleRetry">
            <el-icon><Refresh /></el-icon>
            <span>Retry</span>
          </el-button>
          <el-button type="primary" plain @click="handleViewLogs">
            <el-icon><List /></el-icon>
            <span>Logs</span>
          </el-button>
        </template>
        <template v-else-if="task.status === 'completed'">
          <el-button type="primary" plain @click="handleViewLogs">
            <el-icon><List /></el-icon>
            <span>Logs</span>
          </el-button>
        </template>
      </el-button-group>
    </div>

    <div v-if="isPaused && task.status === 'queued'" class="pause-overlay">
      <div class="pause-content">
        <el-icon><VideoPause /></el-icon>
        <span>Paused</span>
      </div>
    </div>
  </el-card>
</template>

<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from 'vue'
import type { QueueTask } from '../../types/queue'

const props = defineProps<{
  task: QueueTask
  isPaused: boolean
  wasUpdated?: boolean
}>()

const emit = defineEmits<{
  cancel: [taskId: string]
  retry: [taskId: string]
  viewLogs: [taskId: string]
}>()

const statusColor = computed(() => {
  switch (props.task.status) {
    case 'running': return 'var(--brand)'
    case 'queued': return 'var(--info)'
    case 'failed': return 'var(--error)'
    case 'completed': return 'var(--success)'
    case 'cancelled': return 'var(--text-secondary)'
    default: return 'var(--text-secondary)'
  }
})

// Show a meta segment only when it carries content. `expertName` needs a
// non-empty string; `elapsedMs` is a number that is meaningful even at 0 for
// settled (completed/failed) tasks. Queued/cancelled tasks only show elapsed
// time if they actually ran (e.g. cancelled mid-flight).
const hasExpert = computed(() => props.task.expertName != null && props.task.expertName !== '')
const hasElapsed = computed(() => {
  if (props.task.status === 'running') return props.task.startedAt != null
  if (props.task.status === 'completed' || props.task.status === 'failed') return props.task.elapsedMs != null
  return props.task.elapsedMs != null && props.task.elapsedMs > 0
})

const now = ref(Date.now())
let intervalId: ReturnType<typeof setInterval> | null = null

onMounted(() => {
  if (props.task.status === 'running') {
    intervalId = setInterval(() => {
      now.value = Date.now()
    }, 1000)
  }
})

onUnmounted(() => {
  if (intervalId) {
    clearInterval(intervalId)
  }
})

const formattedElapsed = computed(() => {
  const ms = props.task.status === 'running' && props.task.startedAt
    ? now.value - new Date(props.task.startedAt).getTime()
    : props.task.elapsedMs
  const seconds = Math.floor(ms / 1000)
  const minutes = Math.floor(seconds / 60)
  const hours = Math.floor(minutes / 60)
  if (hours > 0) {
    return `${hours}h ${minutes % 60}m ${seconds % 60}s`
  }
  if (minutes > 0) {
    return `${minutes}m ${seconds % 60}s`
  }
  return `${seconds}s`
})

const handleCancel = () => {
  emit('cancel', props.task.id)
}

const handleRetry = () => {
  emit('retry', props.task.id)
}

const handleViewLogs = () => {
  emit('viewLogs', props.task.id)
}
</script>

<style scoped>
.task-card {
  position: relative;
  border: 1px solid var(--border-color);
  max-width: 360px;
  transition: transform 0.2s ease, box-shadow 0.2s ease, opacity 0.2s ease, border-color 0.2s ease;
  overflow: hidden;
}

.task-card:hover {
  transform: translateY(-2px);
  border-color: var(--brand);
  box-shadow: 0 0 0 1px var(--brand), var(--shadow-card);
}

[data-theme="light"] .task-card:hover {
  box-shadow: 0 0 0 1px var(--brand), var(--shadow-card);
}

.task-card.sse-update {
  animation: flash-border 0.6s ease;
}

@keyframes flash-border {
  0% { border-color: var(--brand); }
  100% { border-color: var(--border-color); }
}

.task-header {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-bottom: 8px;
}

.status-dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  flex-shrink: 0;
}

.status-dot.is-running {
  animation: pulse 2s infinite;
}

@keyframes pulse {
  0%, 100% { opacity: 1; }
  50% { opacity: 0.5; }
}

.task-title {
  font-size: 14px;
  font-weight: 500;
  color: var(--text-primary);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  flex: 1;
}

.task-subtitle {
  font-size: 12px;
  color: var(--text-secondary);
  margin-bottom: 12px;
}

.task-progress {
  margin-bottom: 8px;
}

.task-progress :deep(.el-progress-bar__outer) {
  background-color: var(--bg-surface);
  border-radius: 3px;
}

.task-progress :deep(.el-progress-bar__inner) {
  border-radius: 3px;
  transition: width 0.3s ease;
}

.task-progress :deep(.el-progress__text) {
  font-size: 12px;
  color: var(--text-secondary);
  font-family: var(--font-mono);
  min-width: 36px;
}

.task-meta {
  font-family: var(--font-mono);
  font-size: 12px;
  color: var(--text-secondary);
  display: flex;
  gap: 8px;
  align-items: center;
  margin-bottom: 12px;
}

.meta-sep {
  opacity: 0.5;
}

.task-error {
  font-size: 12px;
  color: var(--error);
  border: 1px solid var(--error);
  background-color: var(--bg-surface);
  padding: 8px;
  border-radius: var(--radius-sm);
  margin-bottom: 12px;
  word-break: break-word;
}

.task-actions {
  display: flex;
  justify-content: flex-end;
}

.task-actions .el-button {
  display: inline-flex;
  align-items: center;
  gap: 4px;
}

.pause-overlay {
  position: absolute;
  inset: 0;
  background: rgba(0, 0, 0, 0.3);
  border-radius: var(--radius-md);
  display: flex;
  align-items: center;
  justify-content: center;
  transition: opacity 0.2s ease;
}

.pause-content {
  display: flex;
  align-items: center;
  gap: 8px;
  color: var(--text-primary);
  font-size: 14px;
  font-weight: 600;
}
</style>
