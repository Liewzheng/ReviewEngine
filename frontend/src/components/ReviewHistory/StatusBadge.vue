<script setup lang="ts">
import type { ReviewStatus, ExpertResultStatus } from '../../types/history'
import { Loading } from '@element-plus/icons-vue'

interface Props {
  status: ReviewStatus | ExpertResultStatus
  size?: 'small' | 'default'
}

const props = withDefaults(defineProps<Props>(), {
  size: 'default'
})

const statusMap: Record<string, { type: any; text: string; effect?: string }> = {
  queued: { type: 'info', text: 'Queued' },
  running: { type: 'success', text: 'In Progress', effect: 'plain' },
  completed: { type: 'success', text: 'Completed' },
  failed: { type: 'danger', text: 'Failed' },
  cancelled: { type: 'info', text: 'Cancelled', effect: 'plain' },
  success: { type: 'success', text: 'Success' },
  warning: { type: 'warning', text: 'Warning' },
  error: { type: 'danger', text: 'Error' },
  skipped: { type: 'info', text: 'Skipped', effect: 'plain' },
}

const config = statusMap[props.status] || { type: 'info', text: props.status }
</script>

<template>
  <el-tag
    :type="config.type"
    :effect="config.effect || 'light'"
    :size="size"
    class="status-badge"
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

.is-loading {
  animation: rotating 2s linear infinite;
}

@keyframes rotating {
  from { transform: rotate(0deg); }
  to { transform: rotate(360deg); }
}
</style>
