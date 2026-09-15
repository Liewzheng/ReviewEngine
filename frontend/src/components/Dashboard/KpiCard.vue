<template>
  <el-card class="kpi-card" :body-style="{ padding: '20px' }">
    <div class="kpi-header">
      <el-icon class="kpi-icon" :size="22">
        <component :is="icon" />
      </el-icon>
      <span class="kpi-label">{{ label }}</span>
    </div>
    <div class="kpi-value" :class="{ 'is-empty': props.value === null }">{{ formattedValue }}</div>
    <div v-if="trend != null" class="kpi-trend" :class="trendClass">
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
import { useI18n } from 'vue-i18n'
import { ArrowUp, ArrowDown, Minus } from '@element-plus/icons-vue'
import type { Component } from 'vue'

interface Props {
  label: string
  /** Null when the metric has no data (e.g. no reviews this week) — renders "—". */
  value: number | null
  format?: 'number' | 'percent' | 'duration'
  icon: Component
  /** Null/undefined when the comparison window has no data — renders "—" instead of a fake 0%. */
  trend?: number | null
  trendLabel?: string
}

const { t } = useI18n()

const props = withDefaults(defineProps<Props>(), {
  format: 'number',
})

const formattedValue = computed(() => {
  if (props.value === null) return '—'
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
  if (props.trend == null) return 'kpi-trend-neutral'
  if (props.trend > 0) return 'kpi-trend-up'
  if (props.trend < 0) return 'kpi-trend-down'
  return 'kpi-trend-neutral'
})

const trendIcon = computed(() => {
  if (props.trend == null) return Minus
  if (props.trend > 0) return ArrowUp
  if (props.trend < 0) return ArrowDown
  return Minus
})

const trendText = computed(() => {
  if (props.trend == null) return '—'
  const sign = props.trend > 0 ? '+' : ''
  return `${sign}${props.trend}% ${props.trendLabel ?? t('dashboard.kpis.vsLastWeek')}`
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
  border-color: var(--brand);
  box-shadow: 0 0 0 1px var(--brand), var(--shadow-card);
  transform: translateY(-2px);
}

.kpi-header {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  margin-bottom: var(--space-3);
}

.kpi-icon {
  color: var(--brand);
  background: var(--bg-active);
  padding: var(--space-2);
  border-radius: var(--radius-sm);
}

.kpi-label {
  font-size: 13px;
  color: var(--text-secondary);
  font-weight: 500;
}

.kpi-value {
  font-size: 28px;
  font-weight: 600;
  color: var(--text-primary);
  margin-bottom: var(--space-2);
  font-family: var(--font-mono);
  font-variant-numeric: tabular-nums;
  line-height: 1.2;
}

.kpi-value.is-empty {
  color: var(--text-tertiary);
}

.kpi-trend {
  display: flex;
  align-items: center;
  gap: var(--space-1);
  font-size: 12px;
  font-weight: 500;
  color: var(--text-secondary);
  line-height: 1.4;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  max-width: 100%;
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
