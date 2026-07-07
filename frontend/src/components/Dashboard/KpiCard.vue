<template>
  <el-card class="kpi-card" :body-style="{ padding: '20px' }">
    <div class="kpi-header">
      <el-icon class="kpi-icon" :size="22">
        <component :is="icon" />
      </el-icon>
      <span class="kpi-label">{{ label }}</span>
    </div>
    <div class="kpi-value">{{ formattedValue }}</div>
    <div v-if="trend !== undefined" class="kpi-trend" :class="trendClass">
      <el-icon :size="14">
        <component :is="trendIcon" />
      </el-icon>
      <span>{{ trendText }}</span>
    </div>
    <div v-else class="kpi-trend kpi-trend-neutral">
      <el-icon :size="14"><Minus /></el-icon>
      <span>—</span>
    </div>
  </el-card>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import { ArrowUp, ArrowDown, Minus } from '@element-plus/icons-vue'
import type { Component } from 'vue'

interface Props {
  label: string
  value: number
  format?: 'number' | 'percent' | 'duration'
  icon: Component
  trend?: number
  trendLabel?: string
}

const props = withDefaults(defineProps<Props>(), {
  format: 'number',
  trendLabel: 'vs last week',
})

const formattedValue = computed(() => {
  if (props.format === 'number') {
    return new Intl.NumberFormat().format(props.value)
  }
  if (props.format === 'percent') {
    return `${props.value.toFixed(1)}%`
  }
  if (props.format === 'duration') {
    const mins = Math.floor(props.value / 60000)
    const secs = Math.floor((props.value % 60000) / 1000)
    return `${mins}m ${secs.toString().padStart(2, '0')}s`
  }
  return String(props.value)
})

const trendClass = computed(() => {
  if (props.trend === undefined) return 'kpi-trend-neutral'
  if (props.trend > 0) return 'kpi-trend-up'
  if (props.trend < 0) return 'kpi-trend-down'
  return 'kpi-trend-neutral'
})

const trendIcon = computed(() => {
  if (props.trend === undefined) return Minus
  if (props.trend > 0) return ArrowUp
  if (props.trend < 0) return ArrowDown
  return Minus
})

const trendText = computed(() => {
  if (props.trend === undefined) return '—'
  const sign = props.trend > 0 ? '+' : ''
  return `${sign}${props.trend}% ${props.trendLabel}`
})
</script>

<style scoped>
.kpi-card {
  transition: transform 0.2s ease, box-shadow 0.2s ease;
  animation: kpi-enter 0.3s cubic-bezier(0.4, 0, 0.2, 1) forwards;
  opacity: 0;
  transform: translateY(8px);
}

.kpi-card:hover {
  transform: translateY(-2px);
  box-shadow: 0 8px 12px -1px rgba(0, 0, 0, 0.3), 0 4px 6px -1px rgba(0, 0, 0, 0.2);
}

.kpi-header {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-bottom: 12px;
}

.kpi-icon {
  color: var(--brand);
  background: var(--bg-active);
  padding: 8px;
  border-radius: var(--radius-sm);
}

.kpi-label {
  font-size: 13px;
  color: var(--text-secondary);
  font-weight: 500;
}

.kpi-value {
  font-size: 28px;
  font-weight: 700;
  color: var(--text-primary);
  margin-bottom: 8px;
  font-family: var(--font-mono);
  line-height: 1.2;
}

.kpi-trend {
  display: flex;
  align-items: center;
  gap: 4px;
  font-size: 12px;
  font-weight: 500;
}

.kpi-trend-up {
  color: var(--success);
}

.kpi-trend-down {
  color: var(--error);
}

.kpi-trend-neutral {
  color: var(--text-secondary);
}

@keyframes kpi-enter {
  from {
    opacity: 0;
    transform: translateY(8px);
  }
  to {
    opacity: 1;
    transform: translateY(0);
  }
}
</style>
