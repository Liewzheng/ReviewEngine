<script setup lang="ts">
import { computed } from 'vue'
import { Close } from '@element-plus/icons-vue'

/**
 * The last result of a one-off connectivity test, shown next to the entity
 * that was tested (RENG-54).
 *
 * Shared by the LLM provider cards and the Configuration page's Git-platform
 * rows so both pages present a test result the same way: an outcome tag, the
 * time it ran, and a dismiss control — the one affordance that clears the
 * session state (see `useTransientResult`). The caller owns the wording,
 * because the two probes report different details (latency vs. version).
 */
interface Props {
  /** Tag type for the outcome; `danger` for a failed probe. */
  type: 'success' | 'danger'
  /** Outcome text, already localized by the caller. */
  text: string
  /** ISO timestamp of the probe. */
  at: string
}

const props = defineProps<Props>()

const emit = defineEmits<{
  (e: 'dismiss'): void
}>()

const when = computed(() => new Date(props.at).toLocaleString())
</script>

<template>
  <div class="test-result-line">
    <el-tag :type="type" effect="dark" size="small" class="test-result-tag">
      {{ text }}
    </el-tag>
    <span class="test-result-when">{{ $t('common.lastTest', { date: when }) }}</span>
    <el-button
      class="test-result-dismiss"
      text
      size="small"
      :icon="Close"
      :title="$t('common.dismissTestResult')"
      :aria-label="$t('common.dismissTestResult')"
      @click="emit('dismiss')"
    />
  </div>
</template>

<style scoped>
.test-result-line {
  display: flex;
  align-items: center;
  gap: 8px;
  min-width: 0;
}

.test-result-tag {
  flex-shrink: 0;
  max-width: 100%;
  overflow: hidden;
  text-overflow: ellipsis;
}

.test-result-when {
  font-size: 11px;
  color: var(--text-secondary);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.test-result-dismiss {
  flex-shrink: 0;
  margin-left: auto;
  color: var(--text-secondary);
}
</style>
