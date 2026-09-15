<template>
  <div class="data-table-wrapper">
    <el-table
      :data="data"
      :header-cell-style="headerCellStyle"
      :cell-style="cellStyle"
      :stripe="false"
      :border="false"
      :highlight-current-row="false"
      style="width: 100%"
      v-bind="$attrs"
      @row-click="$emit('row-click', $event)"
    >
      <slot />
    </el-table>
  </div>
</template>

<script setup lang="ts">
import type { CSSProperties } from 'vue'

type TableRow = Record<string, unknown>

interface Props {
  data: TableRow[]
}

defineProps<Props>()

defineEmits<{
  (e: 'row-click', row: TableRow): void
}>()

const headerCellStyle = (): CSSProperties => ({
  backgroundColor: 'var(--bg-card)',
  color: 'var(--text-secondary)',
  fontWeight: 600,
  fontSize: '12px',
  textTransform: 'uppercase',
  letterSpacing: '0.5px',
  padding: '12px 16px',
  borderBottom: '1px solid var(--border-color)',
})

const cellStyle = (): CSSProperties => ({
  padding: '12px 16px',
  borderBottom: '1px solid var(--border-color)',
})
</script>

<style scoped>
.data-table-wrapper :deep(.el-table) {
  --el-table-bg-color: transparent;
  --el-table-tr-bg-color: transparent;
  --el-table-row-hover-bg-color: var(--bg-hover);
  --el-table-border-color: var(--border-color);
  --el-table-header-bg-color: var(--bg-card);
}

.data-table-wrapper :deep(.el-table th) {
  color: var(--text-secondary);
  font-weight: 600;
  font-size: 12px;
  text-transform: uppercase;
  letter-spacing: 0.5px;
}

.data-table-wrapper :deep(.el-table__row) {
  cursor: pointer;
}

.data-table-wrapper :deep(.el-table__row:hover) {
  background-color: var(--bg-hover);
}
</style>
