<script setup lang="ts">
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { CircleCheck, Connection, CopyDocument, Delete, Edit, Rank, Remove } from '@element-plus/icons-vue'
import type { TestResult } from '../../types/llm'
import type { ProviderCardState } from '../../composables/llmPayload'
import type { ContextMenuItem } from './contextMenu'
import {
  EM_DASH,
  cardAccent,
  cardFooter,
  cardVisualState,
  formatUsagePercent,
  statsRow,
  type CardHealth,
  type StatCell,
} from './providerCardState'
import ProviderContextMenu from './ProviderContextMenu.vue'

const props = defineProps<{
  card: ProviderCardState
  /** Runtime health entry for this card (`GET /llm/providers`), if any. */
  health?: CardHealth
  /** Probe failure text from `/system/health` — the card payload has none. */
  probeMessage?: string | null
  /** Usage window the share is taken over; null hides the days. */
  usageWindowDays?: number | null
  /** Manual connectivity-test result of this page session (RENG-54). */
  testResult?: TestResult | null
  saving?: boolean
  /** Position in the grid — the drag payload and the drop target. */
  index: number
}>()

const emit = defineEmits<{
  (e: 'edit'): void
  (e: 'duplicate'): void
  (e: 'test'): void
  (e: 'toggle-disabled'): void
  (e: 'delete'): void
  (e: 'reorder', fromIndex: number, toIndex: number): void
}>()

const { t } = useI18n()

const state = computed(() => cardVisualState(props.card, props.health))
const accent = computed(() => cardAccent(props.card, props.health))
const isDisabled = computed(() => state.value === 'disabled')
const statusLabel = computed(() => t(`llm.status.${state.value}`))

const displayName = computed(() => props.card.provider || EM_DASH)
/** The card's accessible name. The health is a dot now (RENG-77 mock v3), so
 *  the status has to travel in the label instead of in a visible pill. */
const cardAriaLabel = computed(() => `${displayName.value} · ${statusLabel.value}`)
const chainLabel = computed(() =>
  props.health?.chainPosition != null
    ? t('config.providerCards.chainPosition', { n: props.health.chainPosition })
    : '',
)

const stats = computed(() => statsRow(props.health))

/** Label + the unrounded measurement, so the hover tooltip keeps the value
 *  the compact single-token stat had to round. */
function statTooltip(stat: StatCell): string {
  const label = t(stat.labelKey)
  return stat.exact ? `${label} · ${stat.exact}` : label
}
const usagePercent = computed(() => formatUsagePercent(props.health?.usageShare))
const usageLabel = computed(() => {
  const percent = usagePercent.value
  if (percent === null) return null
  const text = percent.toFixed(1)
  return props.usageWindowDays == null
    ? `${text}%`
    : t('llm.usageShare', { percent: text, days: props.usageWindowDays })
})

const footer = computed(() =>
  cardFooter(props.card, props.health, props.probeMessage ?? null, props.testResult ?? null),
)
const footerText = computed(() => {
  switch (footer.value.kind) {
    case 'disabled':
      return t('llm.card.disabled')
    case 'offline':
      return t('llm.status.offline')
    case 'error':
      return footer.value.detail || t('llm.card.probeFailed')
    case 'testOk':
      return t('llm.card.testOk')
    case 'testFail':
      return footer.value.detail || t('llm.card.testFailed')
    default:
      return ''
  }
})
const footerTone = computed(() => {
  const kind = footer.value.kind
  if (kind === 'error' || kind === 'testFail') return 'error'
  if (kind === 'disabled') return 'centered'
  if (kind === 'none') return 'none'
  return 'muted'
})

/**
 * The card's context menu. A disabled card keeps its menu reachable but
 * offers only edit / enable / delete: a stopped card cannot be probed, and
 * its position in the chain is not meaningful until it is enabled again.
 */
const menuItems = computed<ContextMenuItem[]>(() => [
  { key: 'edit', label: t('common.edit'), icon: Edit },
  {
    key: 'duplicate',
    label: t('config.providerCards.duplicate'),
    icon: CopyDocument,
    disabled: isDisabled.value,
  },
  {
    key: 'test',
    label: t('common.testConnection'),
    icon: Connection,
    disabled: isDisabled.value || !props.health,
  },
  {
    key: 'toggle',
    label: isDisabled.value ? t('config.providerCards.enable') : t('config.providerCards.disable'),
    icon: isDisabled.value ? CircleCheck : Remove,
  },
  {
    key: 'delete',
    label: t('config.providerCards.deleteTitle'),
    icon: Delete,
    destructive: true,
    dividerBefore: true,
  },
])

const menuOpen = ref(false)
const menuPosition = ref({ x: 0, y: 0 })

function openMenuAt(x: number, y: number) {
  menuPosition.value = { x, y }
  menuOpen.value = true
}

function onContextMenu(event: MouseEvent) {
  event.preventDefault()
  openMenuAt(event.clientX, event.clientY)
}

/** Left click opens the same menu, so the card is reachable without a
 *  right button (`aria-haspopup="menu"` announces it). A click that ends a
 *  drag is swallowed instead — the drop already moved the card. */
let suppressClick = false

function onCardActivate(event: MouseEvent | KeyboardEvent) {
  if (suppressClick) return
  const rect = (event.currentTarget as HTMLElement | null)?.getBoundingClientRect()
  const x = 'clientX' in event && event.clientX ? event.clientX : (rect?.left ?? 0) + 16
  const y = 'clientY' in event && event.clientY ? event.clientY : (rect?.bottom ?? 0) + 4
  openMenuAt(x, y)
}

function onMenuCommand(item: ContextMenuItem) {
  menuOpen.value = false
  switch (item.key) {
    case 'edit':
      emit('edit')
      break
    case 'duplicate':
      emit('duplicate')
      break
    case 'test':
      emit('test')
      break
    case 'toggle':
      emit('toggle-disabled')
      break
    case 'delete':
      emit('delete')
      break
  }
}

/* ── Drag to reorder ────────────────────────────────────────────────
   The drag handle is the header row (avatar + name). The ghost only appears
   once the pointer has travelled past a small threshold, so a plain click on
   the header still opens the menu instead of teleporting the card. */

let ghost: HTMLElement | null = null
let sourceEl: HTMLElement | null = null
let dragOffset = { x: 0, y: 0 }

function clearDragArtifacts() {
  ghost?.remove()
  ghost = null
  sourceEl?.classList.remove('provider-card--dragging')
  sourceEl = null
  document.querySelectorAll('.provider-card--dragover').forEach((el) => {
    el.classList.remove('provider-card--dragover')
  })
  document.removeEventListener('pointermove', onPointerMove)
  document.removeEventListener('pointerup', onPointerUp)
}

function dropTargetAt(x: number, y: number): HTMLElement | null {
  const el = document.elementFromPoint(x, y) as HTMLElement | null
  const card = el?.closest('.provider-card') as HTMLElement | null
  return card && card !== sourceEl ? card : null
}

function onPointerMove(event: PointerEvent) {
  if (!ghost) return
  ghost.style.left = `${event.clientX - dragOffset.x}px`
  ghost.style.top = `${event.clientY - dragOffset.y}px`
  document.querySelectorAll('.provider-card--dragover').forEach((el) => {
    el.classList.remove('provider-card--dragover')
  })
  dropTargetAt(event.clientX, event.clientY)?.classList.add('provider-card--dragover')
}

function onPointerUp(event: PointerEvent) {
  const target = dropTargetAt(event.clientX, event.clientY)
  const targetIndex = target ? Number(target.dataset.index ?? -1) : -1
  const fromIndex = props.index
  clearDragArtifacts()
  suppressClick = true
  window.setTimeout(() => {
    suppressClick = false
  }, 0)
  if (targetIndex >= 0 && targetIndex !== fromIndex) {
    emit('reorder', fromIndex, targetIndex)
  }
}

function onHeaderPointerDown(event: PointerEvent) {
  if (event.button !== 0 || isDisabled.value) return
  const cardEl = (event.currentTarget as HTMLElement | null)?.closest('.provider-card') as HTMLElement | null
  if (!cardEl) return
  const rect = cardEl.getBoundingClientRect()
  const origin = { x: event.clientX, y: event.clientY }
  let dragging = false

  const onMove = (moveEvent: PointerEvent) => {
    if (!dragging) {
      if (Math.hypot(moveEvent.clientX - origin.x, moveEvent.clientY - origin.y) < 4) return
      dragging = true
      sourceEl = cardEl
      dragOffset = { x: origin.x - rect.left, y: origin.y - rect.top }
      cardEl.classList.add('provider-card--dragging')
      ghost = cardEl.cloneNode(true) as HTMLElement
      ghost.classList.add('provider-card-ghost')
      Object.assign(ghost.style, {
        position: 'fixed',
        left: `${rect.left}px`,
        top: `${rect.top}px`,
        width: `${rect.width}px`,
        height: `${rect.height}px`,
        pointerEvents: 'none',
      })
      document.body.appendChild(ghost)
    }
    onPointerMove(moveEvent)
  }

  const onUp = (upEvent: PointerEvent) => {
    document.removeEventListener('pointermove', onMove)
    document.removeEventListener('pointerup', onUp)
    if (!dragging) {
      clearDragArtifacts()
      return
    }
    onPointerUp(upEvent)
  }

  document.addEventListener('pointermove', onMove)
  document.addEventListener('pointerup', onUp)
}
</script>

<template>
  <article
    class="provider-card"
    :class="[
      `provider-card--${state}`,
      accent === 'none' ? '' : `provider-card--accent-${accent}`,
      { 'is-saving': saving },
    ]"
    :data-index="index"
    :aria-label="cardAriaLabel"
    aria-haspopup="menu"
    role="group"
    tabindex="0"
    @contextmenu="onContextMenu"
    @click="onCardActivate"
    @keydown.enter.prevent="onCardActivate"
  >
    <header class="provider-card__header" @pointerdown="onHeaderPointerDown">
      <span class="provider-card__name" :title="displayName">{{ displayName }}</span>
      <el-tooltip v-if="accent !== 'none'" :content="statusLabel" placement="top" :show-after="200">
        <span
          class="provider-card__health"
          :class="`provider-card__health--${accent}`"
          aria-hidden="true"
        />
      </el-tooltip>
      <span v-if="chainLabel" class="provider-card__chain">{{ chainLabel }}</span>
      <el-icon class="provider-card__grip" aria-hidden="true"><Rank /></el-icon>
    </header>

    <div class="provider-card__row provider-card__row--url" :title="card.apiBaseUrl">
      {{ card.apiBaseUrl || EM_DASH }}
    </div>
    <div class="provider-card__row provider-card__row--model" :title="card.defaultModel">
      {{ card.defaultModel || EM_DASH }}
    </div>
    <div class="provider-card__row provider-card__row--key">**********</div>

    <div class="provider-card__stats">
      <template v-for="(stat, i) in stats" :key="stat.key">
        <span v-if="i > 0" class="provider-card__stat-sep" aria-hidden="true">|</span>
        <el-tooltip :content="statTooltip(stat)" placement="top" :show-after="200">
          <span class="provider-card__stat" :class="{ 'is-empty': stat.empty }">{{ stat.text }}</span>
        </el-tooltip>
      </template>
    </div>

    <div
      v-if="usagePercent !== null"
      class="provider-card__usage"
      role="img"
      :aria-label="usageLabel ?? ''"
      :title="usageLabel ?? ''"
    >
      <div class="provider-card__usage-bar">
        <div class="provider-card__usage-fill" :style="{ width: `${usagePercent}%` }" />
      </div>
    </div>

    <footer class="provider-card__footer" :class="`provider-card__footer--${footerTone}`">
      <span v-if="footerText" class="provider-card__footer-text" :title="footerText">{{ footerText }}</span>
    </footer>

    <ProviderContextMenu
      :visible="menuOpen"
      :position="menuPosition"
      :items="menuItems"
      @close="menuOpen = false"
      @command="onMenuCommand"
    />
  </article>
</template>

<style scoped>
/* RENG-112: a grid item's default `min-width: auto` resolves to its
   min-content width, which for this card is the widest unbreakable string
   — and the footer text uses `white-space: nowrap` on purpose. Without
   `min-width: 0` here, that min-content width propagates into the grid
   track sizing algorithm: the `minmax(320px, 1fr)` track grows to fit the
   footer text (a 281-char probe sentence measures ~1560px), the card
   itself stops being truncated, and the layout blows out across the row.
   Per css-flexbox §4.5 the footer-span's `overflow: hidden` already zeroes
   its own automatic minimum, so the `min-width: 0` belongs on the GRID
   ITEM, not on the flex child. The footer-span rule is still the right
   defence-in-depth — it stops the same propagation when this component is
   laid out outside the grid (e.g. a future single-card detail view). */
.provider-card {
  position: relative;
  display: flex;
  flex-direction: column;
  min-width: 0;
  min-height: 183px;
  padding: var(--space-4);
  background: var(--bg-card);
  border: 1px solid var(--border-color);
  border-radius: var(--radius-md);
  box-shadow: var(--shadow-card);
  cursor: pointer;
  transition: border-color 0.18s ease, box-shadow 0.18s ease;
}

.provider-card:hover {
  border-color: var(--accent-primary);
}

.provider-card:focus-visible {
  outline: 2px solid var(--accent-primary);
  outline-offset: 2px;
}

.provider-card--accent-error {
  border-left: 4px solid var(--accent-error);
}

.provider-card--accent-degraded {
  border-left: 4px solid var(--accent-warning);
}

.provider-card--disabled {
  opacity: 0.55;
  cursor: default;
}

.provider-card--dragging {
  opacity: 0.4;
}

.provider-card--dragover {
  border-color: var(--accent-primary);
  box-shadow: 0 0 0 1px var(--accent-primary), var(--shadow-card);
}

.provider-card__header {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  height: 36px;
  cursor: grab;
}

.provider-card__header:active {
  cursor: grabbing;
}

.provider-card--disabled .provider-card__header {
  cursor: default;
}

.provider-card__name {
  min-width: 0;
  flex: 0 1 auto;
  font-size: 16px;
  font-weight: 600;
  color: var(--text-primary);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

/* RENG-77: the health moved from a right-aligned pill to this dot beside the
   name. Only a card that needs attention carries one — the same rule the
   left-edge accent stripe follows, so a healthy card stays undecorated. */
.provider-card__health {
  flex-shrink: 0;
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: var(--text-tertiary);
}

.provider-card__health--degraded {
  background: var(--accent-warning);
}

.provider-card__health--error {
  background: var(--accent-error);
}

.provider-card__chain {
  flex-shrink: 0;
  font-family: var(--font-mono);
  font-size: 11px;
  color: var(--text-tertiary);
}

/* The mock's top-right affordance: the header IS the drag handle and the
   right-click target, so this is the mark that says "draggable" — decorative,
   never a control of its own. */
.provider-card__grip {
  flex-shrink: 0;
  margin-left: auto;
  font-size: 16px;
  color: var(--text-tertiary);
}

.provider-card__row {
  font-size: 13px;
  line-height: 1.5;
  color: var(--text-secondary);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.provider-card__row--url {
  color: var(--text-secondary);
}

.provider-card__row--model {
  color: var(--text-primary);
}

.provider-card__row--key {
  color: var(--text-tertiary);
  letter-spacing: 1px;
}

.provider-card__stats {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  margin-top: 6px;
  font-size: 13px;
  line-height: 1.5;
  color: var(--text-primary);
  font-variant-numeric: tabular-nums;
}

.provider-card__stat.is-empty {
  color: var(--text-tertiary);
}

.provider-card__stat-sep {
  color: var(--text-tertiary);
}

/* RENG-77 (mock v3): the share is the bar alone — no caption line under it.
   The percentage stays reachable on hover and to assistive tech. */
.provider-card__usage {
  margin-top: var(--space-2);
}

.provider-card__usage-bar {
  height: var(--progress-h-hairline);
  margin: var(--space-1) 0;
  background: var(--bg-surface);
  overflow: hidden;
}

.provider-card__usage-fill {
  height: 100%;
  background: var(--accent-primary);
}

.provider-card--disabled .provider-card__usage-fill {
  background: var(--offline);
}

.provider-card__footer {
  display: flex;
  align-items: center;
  height: 32px;
  margin-top: auto;
  font-size: 12px;
  line-height: 1.3;
}

.provider-card__footer--centered {
  justify-content: center;
  color: var(--text-tertiary);
}

.provider-card__footer--muted {
  color: var(--text-tertiary);
}

.provider-card__footer--error {
  color: var(--accent-error);
}

/* RENG-112: probe and test-failure messages can be long sentences
   (e.g. "service unreachable at https://api.example.com/v1/models: …
   tls handshake eof. The provider never answered, so the key was never
   checked — check api_base, DNS and the network path to 'openai'."),
   so the text must stay on one line and never grow the card. The existing
   :title keeps the full text reachable on hover. The PRIMARY guard is on
   `.provider-card` (the grid item) — see that rule's comment for the full
   reasoning. This span-level rule is defence-in-depth: `min-width: 0` lets
   the flex item shrink below its nowrap content's intrinsic width, and the
   four truncation guards keep the rendered line on one row with an
   ellipsis. Same shape as `.provider-card__name` above. */
.provider-card__footer-text {
  display: block;
  min-width: 0;
  max-width: 100%;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

:global(.provider-card-ghost) {
  z-index: 2000;
  opacity: 0.9;
  filter: drop-shadow(var(--shadow-float));
}
</style>
