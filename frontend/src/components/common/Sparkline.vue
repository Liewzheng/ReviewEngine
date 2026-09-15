<script setup lang="ts">
import { computed } from 'vue'

/**
 * Minimal SVG sparkline for a real time series (RENG-57).
 *
 * The LLM Status card draws the recorded per-bucket average latency with it.
 * Two rules make it honest rather than decorative:
 *
 * - **No data, no line.** `points` may be `null` (the server has no series)
 *   or contain `null` buckets (a bucket with no call). Fewer than two real
 *   points cannot form a trend, so the component renders nothing instead of a
 *   flat zero line — the page then shows `—` as it does for every other
 *   unknown metric.
 * - **Gaps stay gaps.** A missing bucket BREAKS the line (separate
 *   segments) rather than being drawn through, so a quiet hour cannot be read
 *   as a measured value.
 *
 * The geometry is a fixed viewBox stretched to the container with
 * `preserveAspectRatio="none"`; `vector-effect: non-scaling-stroke` keeps the
 * stroke width constant under that stretch.
 */

const props = defineProps<{
  /** Values oldest → newest; `null` entries are gaps. */
  points: (number | null)[] | null
  /** Rendered height in px. */
  height?: number
  /** Stroke colour (defaults to the brand accent). */
  color?: string
  /** Accessible description; the sparkline carries no text of its own. */
  label?: string
}>()

const VIEW_WIDTH = 100

/** Indices of the contiguous runs of real points (each run = one polyline). */
const segments = computed<number[][]>(() => {
  const points = props.points ?? []
  const runs: number[][] = []
  let current: number[] = []
  points.forEach((value, index) => {
    if (value === null) {
      if (current.length) runs.push(current)
      current = []
      return
    }
    current.push(index)
  })
  if (current.length) runs.push(current)
  return runs
})

/** True when there is a trend worth drawing: two or more real points. */
const hasTrend = computed(() => segments.value.some((run) => run.length >= 2))

/** Min/max of the real values, for the vertical scale. */
const range = computed(() => {
  const values = (props.points ?? []).filter((v): v is number => v !== null)
  return { min: Math.min(...values), max: Math.max(...values) }
})

/** Map one point to viewBox coordinates; a flat series sits on the middle. */
function coords(index: number, value: number): { x: number; y: number } {
  const points = props.points ?? []
  const last = Math.max(points.length - 1, 1)
  const { min, max } = range.value
  const span = max - min
  const ratio = span === 0 ? 0.5 : (value - min) / span
  return {
    x: (index / last) * VIEW_WIDTH,
    // Inset by 6 % top and bottom so the extremes are not clipped in half.
    y: 6 + (1 - ratio) * 88,
  }
}

function pathFor(run: number[]): string {
  const points = props.points ?? []
  return run
    .map((index, position) => {
      const { x, y } = coords(index, points[index] as number)
      return `${position === 0 ? 'M' : 'L'}${x.toFixed(2)},${y.toFixed(2)}`
    })
    .join(' ')
}
</script>

<template>
  <svg
    v-if="hasTrend"
    class="sparkline"
    :height="height ?? 28"
    :viewBox="`0 0 ${VIEW_WIDTH} 100`"
    preserveAspectRatio="none"
    role="img"
    :aria-label="label"
  >
    <path
      v-for="(run, i) in segments"
      v-show="run.length >= 2"
      :key="i"
      :d="pathFor(run)"
      :stroke="color ?? 'var(--brand)'"
      stroke-width="1.5"
      fill="none"
      stroke-linecap="round"
      stroke-linejoin="round"
      vector-effect="non-scaling-stroke"
    />
  </svg>
</template>

<style scoped>
.sparkline {
  display: block;
  width: 100%;
  overflow: visible;
}
</style>
