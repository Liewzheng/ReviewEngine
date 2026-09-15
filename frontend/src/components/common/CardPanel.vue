<template>
  <el-card class="card-panel" :body-style="bodyStyle">
    <template v-if="$slots.header" #header>
      <slot name="header" />
    </template>
    <slot />
  </el-card>
</template>

<script setup lang="ts">
import type { StyleValue } from 'vue'

interface Props {
  bodyStyle?: StyleValue
}

withDefaults(defineProps<Props>(), {
  bodyStyle: () => ({ padding: '20px' }),
})</script>

<style scoped>
.card-panel {
  background-color: var(--bg-card);
  border: 1px solid var(--border-color);
  border-radius: var(--radius-md);
  box-shadow: var(--shadow-card);
  transition: border-color 0.2s ease, box-shadow 0.2s ease, transform 0.2s ease;
  overflow: hidden;
}

.card-panel:hover {
  border-color: var(--brand);
  box-shadow: 0 0 0 1px var(--brand), var(--shadow-card);
  transform: translateY(-2px);
}

.card-panel :deep(.el-card__header) {
  padding: 16px 20px;
  border-bottom: 1px solid var(--border-color);
  font-weight: 600;
  font-size: 14px;
  color: var(--text-primary);
}

.card-panel :deep(.el-card__body) {
  padding: 20px;
}
</style>
