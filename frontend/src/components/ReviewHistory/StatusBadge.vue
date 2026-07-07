<script setup lang="ts">
import type { ReviewStatus, ExpertResultStatus } from '../../types/history'
import { computed } from 'vue'
import { Loading } from '@element-plus/icons-vue'

interface Props {
  status: ReviewStatus | ExpertResultStatus
  size?: 'small' | 'default'
  dotOnly?: boolean
}

const props = withDefaults(defineProps<Props>(), {
  size: 'small',
  dotOnly: false,
})

const statusMap: Record<string, { type: any; text: string; effect?: string; customClass?: string }> = {
  queued: { type: 'info', text: 'Queued' },
  running: { type: 'success', text: 'In Progress', effect: 'plain' },
  completed: { type: 'success', text: 'Completed' },
  failed: { type: 'danger', text: 'Failed' },
  cancelled: { type: 'info', text: 'Cancelled', effect: 'plain', customClass: 'status-grey' },
  success: { type: 'success', text: 'Success' },
  warning: { type: 'warning', text: 'Warning' },
  error: { type: 'danger', text: 'Error' },
  skipped: { type: 'info', text: 'Skipped', effect: 'plain', customClass: 'status-grey' },
}

const config = computed(() => statusMap[props.status] || { type: 'info', text: props.status })
</script>

<template>
  <span v-if="dotOnly" class="status-dot" :class="[config.customClass, config.type]">
    <span class="dot-pulse" v-if="status === 'running'"></span>
  </span>
  <el-tag
    v-else
    :type="config.type"
    :effect="config.effect || 'light'"
    :size="size"
    class="status-badge"
    :class="config.customClass"
  >
    <el-icon v-if="status === 'running'" class="is-loading"><Loading /></el-icon>
    {{ config.text }}
  </el-tag>
</template>

<style scoped>
.status-badge {
  display: inline-flex;
  align-items: center;
  gap: 4px;
}

.status-badge.status-grey {
  --el-tag-bg-color: #f4f4f5;
  --el-tag-border-color: #e4e4e7;
  --el-tag-text-color: #71717a;
}

.status-dot {
  display: inline-block;
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: var(--el-tag-bg-color, #909399);
  position: relative;
}

.status-dot.success {
  background: #67c23a;
}

.status-dot.warning {
  background: #e6a23c;
}

.status-dot.danger {
  background: #f56c6c;
}

.status-dot.status-grey,
.status-dot.info {
  background: #909399;
}

.dot-pulse {
  position: absolute;
  inset: 0;
  border-radius: 50%;
  background: inherit;
  animation: pulse 1.5s ease-in-out infinite;
}

.is-loading {
  animation: rotating 2s linear infinite;
}

@keyframes rotating {
  from { transform: rotate(0deg); }
  to { transform: rotate(360deg); }
}

@keyframes pulse {
  0% { transform: scale(1); opacity: 1; }
  70% { transform: scale(2.5); opacity: 0; }
  100% { transform: scale(1); opacity: 0; }
}
</style>
