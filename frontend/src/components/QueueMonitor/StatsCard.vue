<template>
  <el-card class="stats-card" shadow="never">
    <div class="stats-header">
      <div class="stats-icon" :style="iconStyle">
        <el-icon :size="20">
          <component :is="icon" />
        </el-icon>
      </div>
      <div class="stats-info">
        <div class="stats-label">{{ label }}</div>
        <div class="stats-value" :class="{ 'is-flashing': isFlashing }" :style="valueStyle">{{ value }}</div>
      </div>
    </div>
    <el-progress
      :percentage="percentage"
      :color="color"
      :stroke-width="8"
      :show-text="false"
      class="stats-progress"
    />
  </el-card>
</template>

<script setup lang="ts">
import { computed, ref, watch, type Component } from 'vue'

const props = defineProps<{
  label: string
  value: number
  icon: Component
  color: string
  max: number
}>()

const isFlashing = ref(false)
let flashTimeout: ReturnType<typeof setTimeout> | null = null

watch(() => props.value, (newVal, oldVal) => {
  if (typeof oldVal === 'number' && newVal > oldVal) {
    isFlashing.value = true
    if (flashTimeout) clearTimeout(flashTimeout)
    flashTimeout = setTimeout(() => {
      isFlashing.value = false
    }, 200)
  }
})

const percentage = computed(() => {
  if (props.max === 0) return 0
  return Math.min(Math.round((props.value / props.max) * 100), 100)
})

const iconStyle = computed(() => ({
  backgroundColor: `color-mix(in srgb, ${props.color} 12%, transparent)`,
  color: props.color,
}))

const valueStyle = computed(() => ({
  color: props.color,
}))
</script>

<style scoped>
.stats-card {
  border: 1px solid var(--border-color);
  transition: transform 0.2s ease, box-shadow 0.2s ease;
}

.stats-card:hover {
  transform: translateY(-2px);
  border-color: var(--brand);
  box-shadow: 0 0 0 1px var(--brand), var(--shadow-card);
}

[data-theme="light"] .stats-card:hover {
  box-shadow: 0 0 0 1px var(--brand), var(--shadow-card);
}

.stats-header {
  display: flex;
  align-items: center;
  gap: 12px;
  margin-bottom: 12px;
}

.stats-icon {
  width: 40px;
  height: 40px;
  border-radius: var(--radius-sm);
  display: flex;
  align-items: center;
  justify-content: center;
  flex-shrink: 0;
}

.stats-label {
  font-size: 12px;
  color: var(--text-secondary);
  margin-bottom: 4px;
}

.stats-value {
  font-size: 28px;
  font-weight: 600;
  font-family: var(--font-mono);
  transition: color 0.2s ease;
}

.stats-value.is-flashing {
  color: var(--warning) !important;
}

.stats-progress :deep(.el-progress-bar__outer) {
  background-color: var(--bg-surface);
  border-radius: 4px;
}

.stats-progress :deep(.el-progress-bar__inner) {
  border-radius: 4px;
  transition: width 0.3s ease;
}
</style>
