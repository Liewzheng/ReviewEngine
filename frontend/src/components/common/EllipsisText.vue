<script setup lang="ts">
import { ref } from 'vue'

/**
 * One-line text that reveals its full value on hover (RENG-103).
 *
 * The truncation is pure CSS (`text-overflow: ellipsis`), but a tooltip on
 * text that fits is noise, so the bubble is gated on the trigger actually
 * overflowing: `scrollWidth > clientWidth` is measured when the pointer
 * enters the element — the only moment the answer matters — and the tooltip
 * stays disabled until it does overflow. The measured span is a block, so it
 * fills its parent's width; put it inside a width-constrained (flex)
 * container, or it will never overflow.
 */
interface Props {
  /** The text to show; it is also the tooltip's content. */
  text: string
}

defineProps<Props>()

const trigger = ref<HTMLElement | null>(null)
const truncated = ref(false)

function updateTruncated(): void {
  const el = trigger.value
  if (!el) return
  truncated.value = el.scrollWidth > el.clientWidth
}
</script>

<template>
  <el-tooltip
    :content="text"
    placement="top"
    effect="dark"
    :disabled="!truncated"
    :show-after="300"
  >
    <span ref="trigger" class="ellipsis-text" @mouseenter="updateTruncated">{{ text }}</span>
  </el-tooltip>
</template>

<style scoped>
.ellipsis-text {
  display: block;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
</style>
