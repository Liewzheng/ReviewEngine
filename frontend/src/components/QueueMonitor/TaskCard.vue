<template>
  <el-card class="task-card" :class="{ 'is-paused': isPaused && task.status === 'queued' }" shadow="never">
    <div class="task-header">
      <span class="status-dot" :style="{ backgroundColor: statusColor }"></span>
      <span class="task-title" :title="task.mrTitle">{{ task.mrTitle }}</span>
    </div>
    <div class="task-subtitle">{{ task.project }} / {{ task.repository }}</div>

    <el-progress
      :percentage="task.progress"
      :color="statusColor"
      :stroke-width="6"
      :show-text="true"
      class="task-progress"
    />

    <div class="task-meta">
      <span>Expert: {{ task.expertName }}</span>
      <span class="meta-sep">·</span>
      <span>{{ formattedElapsed }}</span>
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
    default: return 'var(--text-secondary)'
  }
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
    ? now.value - new Date(props.task.startedAt).getTime() + props.task.elapsedMs
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
  transition: transform 0.2s ease, box-shadow 0.2s ease, opacity 0.2s ease;
  overflow: hidden;
}

.task-card:hover {
  transform: translateY(-2px);
  box-shadow: 0 8px 12px -2px rgba(0, 0, 0, 0.4), 0 4px 6px -1px rgba(0, 0, 0, 0.3);
}

[data-theme="light"] .task-card:hover {
  box-shadow: 0 8px 12px -2px rgba(0, 0, 0, 0.1), 0 4px 6px -1px rgba(0, 0, 0, 0.06);
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
  backdrop-filter: blur(2px);
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
