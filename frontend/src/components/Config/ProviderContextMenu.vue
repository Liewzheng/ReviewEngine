<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import type { ContextMenuItem } from './contextMenu'

const props = defineProps<{
  visible: boolean
  /** Viewport coordinates of the pointer that opened the menu. */
  position: { x: number; y: number }
  items: ContextMenuItem[]
}>()

const emit = defineEmits<{
  (e: 'close'): void
  (e: 'command', item: ContextMenuItem): void
}>()

const menuEl = ref<HTMLElement | null>(null)
const offset = ref({ x: 0, y: 0 })

/**
 * Keep the menu inside the viewport: it is positioned at the pointer, and a
 * card in the last column would otherwise open a menu off the right edge.
 */
function clampToViewport() {
  const el = menuEl.value
  if (!el) return
  const rect = el.getBoundingClientRect()
  const overflowX = Math.max(0, props.position.x + rect.width - window.innerWidth + 8)
  const overflowY = Math.max(0, props.position.y + rect.height - window.innerHeight + 8)
  offset.value = { x: rect.width ? -overflowX : 0, y: rect.height ? -overflowY : 0 }
}

watch(
  () => props.visible,
  async (open) => {
    if (!open) {
      offset.value = { x: 0, y: 0 }
      return
    }
    await Promise.resolve()
    clampToViewport()
  },
)

const menuStyle = computed(() => ({
  left: `${props.position.x + offset.value.x}px`,
  top: `${props.position.y + offset.value.y}px`,
}))

function onDocumentPointerDown(event: PointerEvent) {
  if (!props.visible) return
  const target = event.target as Node | null
  if (target && menuEl.value?.contains(target)) return
  emit('close')
}

function onDocumentKeydown(event: KeyboardEvent) {
  if (props.visible && event.key === 'Escape') emit('close')
}

onMounted(() => {
  document.addEventListener('pointerdown', onDocumentPointerDown, true)
  document.addEventListener('keydown', onDocumentKeydown)
})

onBeforeUnmount(() => {
  document.removeEventListener('pointerdown', onDocumentPointerDown, true)
  document.removeEventListener('keydown', onDocumentKeydown)
})

function select(item: ContextMenuItem) {
  if (item.disabled) return
  emit('command', item)
}
</script>

<template>
  <Teleport to="body">
    <div
      v-if="visible"
      ref="menuEl"
      class="provider-menu"
      role="menu"
      :style="menuStyle"
      @contextmenu.prevent
    >
      <template v-for="item in items" :key="item.key">
        <div v-if="item.dividerBefore" class="provider-menu__divider" role="separator" />
        <button
          type="button"
          role="menuitem"
          class="provider-menu__item"
          :class="{ 'is-destructive': item.destructive }"
          :disabled="item.disabled"
          @click="select(item)"
        >
          <el-icon v-if="item.icon" :size="14"><component :is="item.icon" /></el-icon>
          <span class="provider-menu__label">{{ item.label }}</span>
        </button>
      </template>
    </div>
  </Teleport>
</template>

<style scoped>
.provider-menu {
  position: fixed;
  z-index: 3000;
  min-width: 148px;
  padding: 4px;
  background: var(--bg-elevated, var(--bg-card));
  border: 1px solid var(--border-color);
  border-radius: var(--radius-md);
  box-shadow: 0 8px 24px rgba(0, 0, 0, 0.35);
}

.provider-menu__item {
  display: flex;
  align-items: center;
  gap: 8px;
  width: 100%;
  padding: 7px 10px;
  background: transparent;
  border: none;
  border-radius: var(--radius-sm);
  color: var(--text-primary);
  font-size: 13px;
  line-height: 1.4;
  text-align: left;
  cursor: pointer;
}

.provider-menu__item:hover:not(:disabled) {
  background: var(--bg-hover);
}

.provider-menu__item:disabled {
  color: var(--text-tertiary, var(--text-secondary));
  cursor: not-allowed;
}

.provider-menu__item.is-destructive {
  color: var(--accent-error, var(--error));
}

.provider-menu__label {
  flex: 1;
  min-width: 0;
}

.provider-menu__divider {
  height: 1px;
  margin: 4px 6px;
  background: var(--border-color);
}
</style>
