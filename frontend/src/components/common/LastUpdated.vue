<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'

/**
 * Low-emphasis "last updated" marker for the auto-refreshing pages (RENG-52).
 *
 * It replaces the manual Refresh buttons: since polling is silent, this text
 * is the page's only liveness signal, so it also carries the failure state —
 * a failed tick keeps the last successful time, switches the marker to the
 * warning color and swaps the wording for the localized "update failed".
 */
interface Props {
  /** ISO timestamp of the last successful fetch; null before the first one. */
  updatedAt: string | null
  /** True when the most recent poll tick failed. */
  failed?: boolean
}

const props = withDefaults(defineProps<Props>(), { failed: false })

const { t, locale } = useI18n()

/** Short wall-clock time in the active locale, e.g. `12:03`. */
const time = computed(() =>
  props.updatedAt
    ? new Date(props.updatedAt).toLocaleTimeString(locale.value, { hour: '2-digit', minute: '2-digit' })
    : '',
)

const text = computed(() => {
  if (props.failed) {
    return time.value ? t('common.updateFailedAt', { time: time.value }) : t('common.updateFailed')
  }
  return time.value ? t('common.lastUpdated', { time: time.value }) : ''
})
</script>

<template>
  <span v-if="text" class="last-updated" :class="{ 'is-failed': failed }">{{ text }}</span>
</template>

<style scoped>
/* Deliberately quiet: smaller than the page title, no icon, no border — it
   reads as metadata, never as a control. */
.last-updated {
  font-size: 12px;
  color: var(--el-text-color-placeholder);
  opacity: 0.6;
  font-family: var(--font-mono);
  white-space: nowrap;
  user-select: none;
}

.last-updated.is-failed {
  color: var(--warning);
  opacity: 0.85;
}
</style>
