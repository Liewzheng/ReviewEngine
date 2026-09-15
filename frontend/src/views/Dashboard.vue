<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted, watch, nextTick } from 'vue'
import { useRouter } from 'vue-router'
import {
  Document,
  Refresh,
  Check,
  Timer,
  TrendCharts,
  FirstAidKit,
  InfoFilled,
  ArrowRight,
} from '@element-plus/icons-vue'
import { ElNotification } from 'element-plus'
import { useI18n } from 'vue-i18n'
import {
  createChart,
  LineSeries,
  HistogramSeries,
  LineStyle,
  CrosshairMode,
  TickMarkType,
  type IChartApi,
  type ISeriesApi,
  type MouseEventParams,
  type AutoscaleInfo,
  type Time,
  type UTCTimestamp,
} from 'lightweight-charts'
import { useDashboard } from '../composables/useDashboard'
import { useTheme } from '../composables/useTheme'
import { CHART_PALETTE_FALLBACKS, CHART_SERIES_FALLBACK } from '../chartPalette'
import KpiCard from '../components/Dashboard/KpiCard.vue'
import StatusBadge from '../components/Dashboard/StatusBadge.vue'
import CardPanel from '../components/common/CardPanel.vue'
import PageHeader from '../components/common/PageHeader.vue'
import LastUpdated from '../components/common/LastUpdated.vue'
import type { KpiData, TrendPoint, SystemHealth, RecentReview } from '../types/dashboard'

const router = useRouter()
const { t } = useI18n()
const dashboard = useDashboard()
// The app's shared theme state (RENG-51): a light/dark switch re-applies the
// chart palette, which is resolved to concrete canvas colors (see `chartTheme`).
const { isDark } = useTheme()

// Loading & refresh state. `lastUpdated` / `pollFailed` come from the
// composable so the header marker advances on every successful 60s poll tick,
// not only on the initial load (RENG-52).
const loading = dashboard.loading
const lastUpdated = dashboard.lastUpdated
const pollFailed = dashboard.pollFailed

// Data refs (computed from composable)
const kpis = computed<KpiData | null>(() => dashboard.data.value?.kpis ?? null)
const trend = computed<TrendPoint[]>(() => dashboard.data.value?.trend ?? [])
const trendDaily = computed<TrendPoint[]>(() => dashboard.data.value?.trendDaily ?? [])
const health = computed<SystemHealth | null>(() => dashboard.data.value?.health ?? null)
const recentReviews = computed<RecentReview[]>(() => dashboard.data.value?.recentReviews ?? [])

// ─── Trend granularity toggle (RENG-50) ─────────────
// Session-only state (plain ref, no persistence); both series arrive in the
// same /dashboard payload, so switching never refetches.
type TrendMode = 'hourly' | 'daily'
const trendMode = ref<TrendMode>('hourly')
const trendModeOptions = computed(() => [
  { label: t('dashboard.trend.view24h'), value: 'hourly' },
  { label: t('dashboard.trend.viewDaily'), value: 'daily' },
])
/** Series rendered for the active granularity. */
const activeTrend = computed<TrendPoint[]>(() =>
  trendMode.value === 'daily' ? trendDaily.value : trend.value,
)
/** Footer total follows the visible window (24h sum vs 14-day sum). */
const activeTrendTotal = computed(() => activeTrend.value.reduce((sum, p) => sum + p.value, 0))
const hasTrendData = computed(() => trend.value.length > 0 || trendDaily.value.length > 0)

// Chart refs
const chartContainer = ref<HTMLElement | null>(null)
const tooltipVisible = ref(false)
const tooltipLabel = ref('')
const tooltipCount = ref(0)
const tooltipBelow = ref(false)
const tooltipStyle = computed(() => ({ left: `${tooltipX.value}px`, top: `${tooltipY.value}px` }))
const tooltipX = ref(0)
const tooltipY = ref(0)
let chart: IChartApi | null = null
let activeSeries: ISeriesApi<'Line'> | ISeriesApi<'Histogram'> | null = null
let resizeObserver: ResizeObserver | null = null

// ─── Error Handling ─────────────────────────────────

watch(() => dashboard.error.value, (err) => {
  if (err) {
    ElNotification({
      title: t('common.error'),
      message: err,
      type: 'error',
      duration: 5000,
    })
  }
})

// ─── Formatters ─────────────────────────────────────

function formatDuration(ms: number): string {
  const mins = Math.floor(ms / 60000)
  const secs = Math.floor((ms % 60000) / 1000)
  return `${mins}m ${secs.toString().padStart(2, '0')}s`
}

function timeAgo(iso: string): string {
  const diff = Date.now() - new Date(iso).getTime()
  const mins = Math.floor(diff / 60000)
  if (mins < 1) return t('dashboard.time.justNow')
  if (mins < 60) return t('dashboard.time.minAgo', { n: mins })
  const hrs = Math.floor(mins / 60)
  if (hrs < 24) return t('dashboard.time.hoursAgo', { n: hrs })
  return t('dashboard.time.daysAgo', { n: Math.floor(hrs / 24) })
}

function formatTime(iso: string): string {
  return new Date(iso).toLocaleString('en-US', {
    month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit',
  })
}

// Table cell styling previously provided by the DataTable wrapper component;
// inlined here so the table can live in the same SFC as its columns (see the
// recent-reviews table below — cross-SFC slot columns break under on-demand
// element-plus import).
function headerCellStyle(): Record<string, string> {
  return {
    backgroundColor: 'var(--bg-card)',
    color: 'var(--text-secondary)',
    fontWeight: '600',
    fontSize: '12px',
    textTransform: 'uppercase',
    letterSpacing: '0.5px',
    padding: 'var(--space-3) var(--space-4)',
    borderBottom: '1px solid var(--border-color)',
  }
}

function cellStyle(): Record<string, string> {
  return {
    padding: '0 var(--space-3)',
    borderBottom: '1px solid var(--border-color)',
  }
}

// ─── Lightweight Charts ───────────────────────────────

const CHART_HEIGHT = 280

function pad2(n: number): string {
  return n.toString().padStart(2, '0')
}

/** 24H tooltip / hour-range label: the bucket point sits at the window END. */
function formatHour(ts: number): string {
  return `${pad2(new Date(ts * 1000).getHours())}:00`
}

/** Daily tooltip label: full local date, `2026-09-09` style. */
function formatDailyDate(ts: number): string {
  const d = new Date(ts * 1000)
  return `${d.getFullYear()}-${pad2(d.getMonth() + 1)}-${pad2(d.getDate())}`
}

/** Daily axis labels: sparse `M/D` (9/1) date ticks. */
function formatDailyTick(time: Time, tickMarkType: TickMarkType): string | null {
  if (tickMarkType !== TickMarkType.DayOfMonth && tickMarkType !== TickMarkType.Month) return null
  const d = new Date((time as UTCTimestamp) * 1000)
  return `${d.getMonth() + 1}/${d.getDate()}`
}

/**
 * Pin the Y axis floor to 0 on both views so all-zero windows hug the floor
 * instead of the library's degenerate ±ε auto-range around 0. lightweight-
 * charts has no price-scale `min` option — a series autoscale provider
 * returning `priceRange.minValue = 0` is the supported mechanism.
 */
function floorAutoscale(): (base: () => AutoscaleInfo | null) => AutoscaleInfo | null {
  return (base) => {
    const info = base()
    if (!info?.priceRange) return info
    const max = info.priceRange.maxValue
    return {
      priceRange: { minValue: 0, maxValue: max > 0 ? max : 1 },
      margins: { above: 10, below: 0 },
    }
  }
}

/**
 * Canvas colors must be CONCRETE values: lightweight-charts paints on canvas
 * and parses colors on a detached scratch context, which cannot resolve CSS
 * `var(--…)` references — invalid strings are silently dropped and the chart
 * renders with near-black defaults. Resolve the theme vars through
 * getComputedStyle whenever the palette is applied (chart init and every
 * theme switch), falling back to the dark palette mirrored in `chartPalette.ts`.
 */
function resolveChartColor(varName: string): string {
  const value = getComputedStyle(document.documentElement).getPropertyValue(varName).trim()
  return value || CHART_PALETTE_FALLBACKS[varName] || CHART_SERIES_FALLBACK
}

/**
 * The theme-derived chart configuration, re-resolved from the live CSS vars
 * on every call. Shared by `initChart` (which adds the static, non-theme
 * options) and `applyChartTheme` (RENG-51: a light/dark switch re-applies
 * this fragment, keeping the series, its data and the pinned scale options
 * that `initChart` set).
 */
function chartTheme() {
  const daily = trendMode.value === 'daily'
  const gridColor = resolveChartColor('--chart-grid')
  const textColor = resolveChartColor('--chart-text')
  return {
    daily,
    seriesColor: resolveChartColor(daily ? '--chart-bar' : '--chart-line'),
    options: {
      layout: {
        background: { color: 'transparent' },
        textColor,
        attributionLogo: false,
      },
      // Readability (RENG-50): clearly visible medium-gray horizontal grid, no
      // vertical clutter, high-contrast tick text (palette in style.css).
      grid: {
        vertLines: { visible: false, color: gridColor },
        horzLines: { color: gridColor, style: LineStyle.Solid },
      },
      crosshair: {
        mode: CrosshairMode.Magnet,
        vertLine: { color: gridColor },
        horzLine: { color: gridColor },
      },
      rightPriceScale: { borderColor: gridColor },
      timeScale: { borderColor: gridColor },
    },
  }
}

function initChart() {
  if (!chartContainer.value) return
  if (chart) {
    chart.remove()
    chart = null
    activeSeries = null
  }
  resizeObserver?.disconnect()
  resizeObserver = null

  const { daily, seriesColor, options } = chartTheme()
  chart = createChart(chartContainer.value, {
    ...options,
    timeScale: {
      ...options.timeScale,
      timeVisible: !daily,
      tickMarkFormatter: daily ? formatDailyTick : undefined,
    },
    localization: {
      timeFormatter: daily ? (time: UTCTimestamp) => formatDailyDate(time) : undefined,
    },
    handleScroll: false,
    handleScale: false,
    width: chartContainer.value.clientWidth,
    height: CHART_HEIGHT,
  })

  // No native rounded-top histograms in lightweight-charts v5 — a bright
  // solid bar in the accent's violet family instead (no canvas hacks).
  // priceFormat pins Y ticks to integers (counts, never "50.00").
  const integerTicks = { type: 'price', precision: 0, minMove: 1 } as const
  activeSeries = daily
    ? chart.addSeries(HistogramSeries, {
        color: seriesColor,
        priceFormat: integerTicks,
        lastValueVisible: false,
        priceLineVisible: false,
        autoscaleInfoProvider: floorAutoscale(),
      })
    : chart.addSeries(LineSeries, {
        color: seriesColor,
        lineWidth: 2,
        priceFormat: integerTicks,
        lastValueVisible: false,
        priceLineVisible: false,
        crosshairMarkerVisible: true,
        crosshairMarkerRadius: 4,
        crosshairMarkerBorderColor: seriesColor,
        crosshairMarkerBackgroundColor: resolveChartColor('--bg-primary'),
        autoscaleInfoProvider: floorAutoscale(),
      })

  chart.subscribeCrosshairMove(onCrosshairMove)
  updateChartData()

  resizeObserver = new ResizeObserver(() => {
    if (chart && chartContainer.value) {
      chart.applyOptions({ width: chartContainer.value.clientWidth, height: CHART_HEIGHT })
    }
  })
  resizeObserver.observe(chartContainer.value)
}

/**
 * Re-resolve the palette and apply it to the live chart (RENG-51). The colors
 * are read once at init (a canvas chart cannot follow a CSS-variable change on
 * its own), so a light/dark switch would otherwise leave the chart on the
 * previous theme's palette until the granularity toggle rebuilt it.
 *
 * Re-applying options is enough: the series type, its data, the crosshair
 * subscription, the integer Y ticks (`priceFormat`), the Y-floor autoscale
 * provider, the hidden last-value badge / price line and `fitContent` are all
 * untouched. Theme switches are rare, so there is no per-frame cost.
 */
function applyChartTheme() {
  if (!chart) return
  const { daily, seriesColor, options } = chartTheme()
  chart.applyOptions(options)
  if (!activeSeries) return
  if (daily) {
    ;(activeSeries as ISeriesApi<'Histogram'>).applyOptions({ color: seriesColor })
  } else {
    ;(activeSeries as ISeriesApi<'Line'>).applyOptions({
      color: seriesColor,
      crosshairMarkerBorderColor: seriesColor,
      crosshairMarkerBackgroundColor: resolveChartColor('--bg-primary'),
    })
  }
}

function updateChartData() {
  if (!chart || !activeSeries || !activeTrend.value.length) return
  const data = activeTrend.value.map((p) => ({
    time: p.time as UTCTimestamp,
    value: p.value,
  }))
  activeSeries.setData(data)
  // Fit the visible range to the data: the default time-scale state shows a
  // fixed ~150 bars of history (default bar spacing), leaving the series
  // right-clustered in a mostly empty plot when the window has fewer points.
  chart.timeScale().fitContent()
}

function hideTooltip() {
  tooltipVisible.value = false
}

/**
 * Owner-spec hover card: window label left, review count right, dark rounded
 * floating card. Positioned above the crosshair point (flips below near the
 * top edge), clamped horizontally inside the chart.
 */
function onCrosshairMove(param: MouseEventParams) {
  if (!chartContainer.value || !activeSeries) return
  const point = param.seriesData.get(activeSeries) as { value: number } | undefined
  if (!param.time || !param.point || !point) {
    hideTooltip()
    return
  }
  const time = param.time as UTCTimestamp
  tooltipLabel.value =
    trendMode.value === 'daily'
      ? formatDailyDate(time)
      : `${formatHour(time - 3600)} – ${formatHour(time)}`
  tooltipCount.value = point.value

  const width = chartContainer.value.clientWidth
  tooltipX.value = Math.max(96, Math.min(param.point.x, width - 96))
  tooltipBelow.value = param.point.y < 56
  tooltipY.value = tooltipBelow.value ? param.point.y + 18 : param.point.y - 12
  tooltipVisible.value = true
}

// Poll update: refresh the ACTIVE series' data in place (no re-init, no
// refetch — both series ride the same /dashboard payload).
watch(
  activeTrend,
  () => {
    nextTick(() => {
      if (!chart) initChart()
      else updateChartData()
    })
  },
  { deep: true },
)

// Granularity switch: rebuild the chart (series type, time scale and axis
// formatting differ between the two views).
watch(trendMode, () => {
  hideTooltip()
  nextTick(initChart)
})

// Theme switch (RENG-51): the palette is resolved to concrete canvas colors
// at init, so re-apply it once the new theme is on <html> — no rebuild
// needed, and no per-frame work (theme switches are rare).
watch(isDark, () => {
  nextTick(applyChartTheme)
})

// ─── Table Helpers ──────────────────────────────────

function statusToBadgeStatus(status: RecentReview['status']) {
  switch (status) {
    case 'success': return 'success'
    case 'failed': return 'error'
    case 'running': return 'running'
    case 'queued': return 'queued'
    case 'completed': return 'completed'
    case 'cancelled': return 'cancelled'
    default: return 'offline'
  }
}

function statusLabel(status: RecentReview['status']): string {
  switch (status) {
    case 'success': return t('common.status.completed')
    case 'failed': return t('common.status.failed')
    case 'running': return t('common.status.inProgress')
    case 'queued': return t('common.status.queued')
    case 'completed': return t('common.status.completed')
    case 'cancelled': return t('common.status.cancelled')
  }
}

function onRowClick(row: RecentReview) {
  router.push({ path: '/history', query: { reviewId: row.id } })
}

// ─── Lifecycle ──────────────────────────────────────
// The single 60s background poll lives in `useDashboard` (silent — it never
// flips `loading`, so no per-minute skeleton flash) and feeds `lastUpdated` /
// `pollFailed` for the header marker. The page only drives the initial load
// through `dashboard.refresh`, and owns the chart lifecycle.

onMounted(() => {
  dashboard.refresh()
})

onUnmounted(() => {
  resizeObserver?.disconnect()
  resizeObserver = null
  if (chart) {
    chart.remove()
    chart = null
    activeSeries = null
  }
})
</script>

<template>
  <div class="dashboard-page">
    <!-- Page Header -->
    <PageHeader :title="$t('dashboard.title')" :subtitle="$t('dashboard.subtitle')">
      <template #actions>
        <LastUpdated :updated-at="lastUpdated" :failed="pollFailed" />
      </template>
    </PageHeader>

    <!-- Row 1: KPI Cards -->
    <div class="kpi-grid">
      <template v-if="loading">
        <el-skeleton v-for="i in 4" :key="i" animated class="kpi-skeleton">
          <template #template>
            <el-skeleton-item variant="circle" style="width: 40px; height: 40px; margin-bottom: var(--space-3);" />
            <el-skeleton-item variant="text" style="width: 60%; height: 20px; margin-bottom: var(--space-2);" />
            <el-skeleton-item variant="text" style="width: 40%; height: 14px;" />
          </template>
        </el-skeleton>
      </template>
      <template v-else-if="kpis">
        <KpiCard
          :label="$t('dashboard.kpis.reviewsThisWeek')"
          :value="kpis.reviewsThisWeek"
          format="number"
          :icon="Document"
          :trend="kpis.reviewsTrend"
          :trend-label="$t('dashboard.kpis.vsLastWeek')"
          style="animation-delay: 0ms"
        />
        <KpiCard
          :label="$t('dashboard.kpis.activeQueue')"
          :value="kpis.activeQueue"
          format="number"
          :icon="Refresh"
          style="animation-delay: 50ms"
        />
        <KpiCard
          :label="$t('dashboard.kpis.successRate')"
          :value="kpis.successRate"
          format="percent"
          :icon="Check"
          :trend="kpis.successTrend"
          :trend-label="$t('dashboard.kpis.vsYesterday')"
          style="animation-delay: 100ms"
        />
        <KpiCard
          :label="$t('dashboard.kpis.avgDuration')"
          :value="kpis.avgDurationMs"
          format="duration"
          :icon="Timer"
          :trend="kpis.durationTrend"
          :trend-label="$t('dashboard.kpis.vsLastWeek')"
          style="animation-delay: 150ms"
        />
      </template>
    </div>

    <!-- Row 2: Trend + Health -->
    <div class="row-two">
      <!-- Activity Trend (24H / daily granularity toggle) -->
      <CardPanel :body-style="{ padding: '0' }">
        <template #header>
          <div class="card-header">
            <div class="card-header-left">
              <el-icon :size="18"><TrendCharts /></el-icon>
              <span>{{ $t('dashboard.trend.title') }}</span>
            </div>
            <el-segmented
              v-model="trendMode"
              :options="trendModeOptions"
              size="small"
              class="trend-mode-switch"
            />
          </div>
        </template>
        <div class="trend-body">
          <el-skeleton v-if="loading" :rows="5" animated />
          <template v-else-if="hasTrendData">
            <div class="chart-wrap">
              <div ref="chartContainer" class="chart-container" />
              <div
                v-show="tooltipVisible"
                class="trend-tooltip"
                :class="{ below: tooltipBelow }"
                :style="tooltipStyle"
              >
                <span class="trend-tooltip-label">{{ tooltipLabel }}</span>
                <span class="trend-tooltip-value">{{ $t('dashboard.trend.tooltipReviews', { count: tooltipCount }) }}</span>
              </div>
            </div>
            <div class="trend-summary">
              <span class="trend-total">{{ $t('dashboard.trend.total', { count: activeTrendTotal }) }}</span>
            </div>
          </template>
          <div v-else class="trend-empty">
            <el-icon :size="32"><InfoFilled /></el-icon>
            <p>{{ $t('dashboard.trend.empty') }}</p>
          </div>
        </div>
      </CardPanel>

      <!-- System Health -->
      <CardPanel :body-style="{ padding: '0' }">
        <template #header>
          <div class="card-header">
            <div class="card-header-left">
              <el-icon :size="18"><FirstAidKit /></el-icon>
              <span>{{ $t('dashboard.health.title') }}</span>
            </div>
          </div>
        </template>
        <div class="health-body">
          <el-skeleton v-if="loading" :rows="6" animated />
          <template v-else-if="health">
            <!-- Integrations -->
            <div class="health-section">
              <div class="health-section-title">{{ $t('dashboard.health.integrations') }}</div>
              <div
                v-for="(item, idx) in health.integrations"
                :key="item.service"
                class="health-row"
                :class="{ 'last-row': idx === health.integrations.length - 1 }"
              >
                <div class="health-row-left">
                  <span class="health-service">{{ item.service }}</span>
                </div>
                <div class="health-row-right">
                  <StatusBadge :status="item.status" show-text size="small" />
                </div>
              </div>
            </div>

            <!-- LLM Providers -->
            <div class="health-section">
              <div class="health-section-title">{{ $t('dashboard.health.llmProviders') }}</div>
              <div
                v-for="(item, idx) in health.llmProviders"
                :key="item.service"
                class="health-row"
                :class="{ 'last-row': idx === health.llmProviders.length - 1 }"
              >
                <div class="health-row-left">
                  <span class="health-service">{{ item.service }}</span>
                </div>
                <div class="health-row-right">
                  <StatusBadge :status="item.status" show-text size="small" />
                  <span v-if="item.message" class="health-latency">{{ item.message }}</span>
                </div>
              </div>
            </div>

            <!-- Overall -->
            <div class="health-overall">
              <StatusBadge :status="health.overall" size="large" />
              <span class="health-overall-text">
                {{ health.overall === 'success' ? $t('dashboard.health.allOperational') : health.overall === 'warning' ? $t('dashboard.health.someDegraded') : $t('dashboard.health.errorsDetected') }}
              </span>
            </div>
          </template>
        </div>
      </CardPanel>
    </div>

    <!-- Row 3: Recent Activity Table -->
    <CardPanel :body-style="{ padding: '0' }">
      <template #header>
        <div class="card-header">
          <div class="card-header-left">
            <el-icon :size="18"><Document /></el-icon>
            <span>{{ $t('dashboard.recent.title') }}</span>
          </div>
          <router-link to="/history" class="view-all-link">
            {{ $t('dashboard.recent.viewAll') }} <el-icon :size="12"><ArrowRight /></el-icon>
          </router-link>
        </div>
      </template>
      <div class="recent-body">
        <el-skeleton v-if="loading" :rows="5" animated />
        <template v-else-if="recentReviews.length > 0">
          <div class="table-wrapper">
            <el-table
              :data="recentReviews"
              :header-cell-style="headerCellStyle"
              :cell-style="cellStyle"
              :stripe="false"
              :border="false"
              :highlight-current-row="false"
              style="width: 100%"
              @row-click="onRowClick"
            >
              <el-table-column :label="$t('history.columns.mrTitle')" min-width="200">
                <template #default="{ row }">
                  <div class="mr-title-cell">
                    <span class="mr-title-text">{{ row.mrTitle }}</span>
                    <el-tag size="small" type="info" effect="dark">{{ row.project }}</el-tag>
                  </div>
                </template>
              </el-table-column>

              <!-- RENG-45: `history.columns.author` now reads "Participants"
                   (the history table's stacked cell); this table still shows
                   one author, so it uses the plain `authorName` label. -->
              <el-table-column :label="$t('history.columns.authorName')" width="140">
                <template #default="{ row }">
                  <div class="author-cell">
                    <div class="author-avatar">{{ (row.author?.name || '?').charAt(0) }}</div>
                    <span>{{ row.author?.name || $t('common.unknown') }}</span>
                  </div>
                </template>
              </el-table-column>

              <el-table-column :label="$t('history.columns.status')" width="108">
                <template #default="{ row }">
                  <!-- Cell padding stacks: 16px from cellStyle on the td plus
                       Element Plus' default 12px on .cell, leaving only ~44px
                       of content width in a 100px column — "已完成" wrapped
                       as "已完/成". Nowrap the cell and widen the column to
                       match the history table (see commit 8aa1740). -->
                  <div class="status-cell">
                    <StatusBadge :status="statusToBadgeStatus(row.status)" :show-text="false" size="small" />
                    <span class="status-label">{{ statusLabel(row.status) }}</span>
                  </div>
                </template>
              </el-table-column>

              <el-table-column :label="$t('history.columns.duration')" width="100">
                <template #default="{ row }">
                  <span class="mono-text">{{ formatDuration(row.durationMs) }}</span>
                </template>
              </el-table-column>

              <el-table-column :label="$t('history.columns.time')" width="160">
                <template #default="{ row }">
                  <el-tooltip :content="formatTime(row.createdAt)" placement="top" effect="dark">
                    <span class="mono-text">{{ timeAgo(row.createdAt) }}</span>
                  </el-tooltip>
                </template>
              </el-table-column>
            </el-table>
          </div>
        </template>
        <div v-else class="recent-empty">
          <el-icon :size="32"><InfoFilled /></el-icon>
          <p>{{ $t('dashboard.recent.empty') }}</p>
        </div>
      </div>
    </CardPanel>
  </div>
</template>

<style scoped>
.dashboard-page {
  max-width: 1400px;
  margin: 0 auto;
}

/* KPI Cards */
.kpi-grid {
  display: grid;
  grid-template-columns: repeat(4, 1fr);
  gap: var(--space-4);
  margin-bottom: var(--space-5);
}

.kpi-skeleton {
  background: var(--bg-card);
  border-radius: var(--radius-md);
  padding: 20px;
  border: 1px solid var(--border-color);
  box-shadow: var(--shadow-card);
}

/* Row 2 */
.row-two {
  display: grid;
  grid-template-columns: 7fr 3fr;
  gap: var(--space-4);
  margin-bottom: var(--space-5);
}

/* Card Header */
.card-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
}

.card-header-left {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  font-weight: 600;
  font-size: 14px;
  color: var(--text-primary);
}

.view-all-link {
  display: flex;
  align-items: center;
  gap: var(--space-1);
  font-size: 12px;
  font-weight: 500;
  color: var(--brand);
}

.view-all-link:hover {
  color: var(--brand-hover);
}

/* Trend Chart */
.trend-body {
  padding: var(--space-4) 20px 20px;
}

.chart-wrap {
  position: relative;
}

.chart-container {
  height: 280px;
  width: 100%;
}

/* Owner-spec hover card: dark rounded floating card, window label left,
   review count right. Never intercepts mouse (the crosshair drives it). */
.trend-tooltip {
  position: absolute;
  transform: translate(-50%, -100%);
  display: flex;
  align-items: center;
  gap: var(--space-3);
  padding: 6px var(--space-3);
  background: var(--bg-tooltip);
  border: 1px solid var(--border-color);
  border-radius: var(--radius-md);
  box-shadow: var(--shadow-card);
  font-size: 12px;
  white-space: nowrap;
  pointer-events: none;
  /* above the chart canvases and the (now hidden) attribution-logo layer */
  z-index: 30;
}

.trend-tooltip.below {
  transform: translate(-50%, 0);
}

.trend-tooltip-label {
  color: var(--text-secondary);
}

.trend-tooltip-value {
  color: var(--text-primary);
  font-weight: 600;
  font-family: var(--font-mono);
}

/* Granularity toggle: capsule container, highlighted active segment. */
.trend-mode-switch :deep(.el-segmented) {
  --el-segmented-bg-color: var(--bg-surface);
  --el-segmented-color: var(--text-secondary);
  --el-segmented-item-hover-bg-color: var(--bg-hover);
  --el-segmented-item-hover-color: var(--text-primary);
  --el-segmented-item-selected-bg-color: var(--brand);
  --el-segmented-item-selected-color: var(--text-on-accent);
  border-radius: var(--radius-pill);
}

.trend-mode-switch :deep(.el-segmented__item-selected) {
  border-radius: var(--radius-pill);
}

.trend-summary {
  margin-top: var(--space-3);
  text-align: center;
  font-size: 12px;
  color: var(--text-secondary);
  font-family: var(--font-mono);
}

.trend-empty, .recent-empty {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  padding: 40px 20px;
  color: var(--text-secondary);
  gap: var(--space-2);
}

.trend-empty p, .recent-empty p {
  margin: 0;
  font-size: 14px;
}

/* Health Card */
.health-body {
  padding: var(--space-3) 20px var(--space-4);
}

.health-section {
  margin-bottom: var(--space-4);
}

.health-section-title {
  font-size: 11px;
  font-weight: 600;
  text-transform: uppercase;
  letter-spacing: 0.5px;
  color: var(--text-secondary);
  margin-bottom: var(--space-1);
  padding-left: var(--space-1);
}

.health-row {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 10px 0;
  border-bottom: 1px solid var(--border-color);
}

.health-row.last-row {
  border-bottom: none;
}

.health-service {
  font-size: 13px;
  color: var(--text-primary);
  font-weight: 500;
}

.health-row-right {
  display: flex;
  align-items: center;
  gap: 10px;
}

.health-latency {
  font-size: 12px;
  color: var(--text-secondary);
  font-family: var(--font-mono);
  min-width: 48px;
  text-align: right;
}

.health-overall {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: var(--space-2);
  padding: var(--space-3) 0;
  border-top: 1px solid var(--border-color);
  margin-top: var(--space-1);
}

.health-overall-text {
  font-size: 13px;
  font-weight: 600;
  color: var(--text-primary);
}

/* Table */
.recent-body {
  padding: 0;
}

.table-wrapper {
  overflow-x: auto;
}

:deep(.el-table) {
  --el-table-bg-color: transparent;
  --el-table-tr-bg-color: transparent;
}

:deep(.el-table__row) {
  cursor: pointer;
  height: 48px;
}

:deep(.el-table__cell) {
  height: 48px;
  vertical-align: middle;
}

.mr-title-cell {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  min-width: 0;
  overflow: hidden;
}

.mr-title-text {
  font-size: 13px;
  color: var(--text-primary);
  font-weight: 500;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  min-width: 0;
  flex: 1;
}

/* Status cell: badge dot + label must stay on one line; Element Plus'
   default .cell has word-break: break-all, which split "已完成" into
   "已完/成" once the stacked cell padding squeezed the content width. */
.status-cell {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  max-width: 100%;
}

.status-label {
  font-size: 12px;
  color: var(--text-primary);
  /* Same truncation recipe as the history table (8aa1740): labels that
     still don't fit (e.g. ja "キャンセル済み") ellipsize instead of
     wrapping or being hard-clipped. */
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.author-cell {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  font-size: 13px;
  color: var(--text-primary);
}

.author-avatar {
  width: 24px;
  height: 24px;
  border-radius: 50%;
  background: var(--brand);
  color: var(--text-on-accent);
  display: flex;
  align-items: center;
  justify-content: center;
  font-size: 11px;
  font-weight: 600;
  flex-shrink: 0;
}

.mono-text {
  font-family: var(--font-mono);
  font-size: 12px;
  color: var(--text-secondary);
}

/* Responsive */
@media (max-width: 1279px) {
  .row-two {
    grid-template-columns: 3fr 2fr;
  }
}

@media (max-width: 1023px) {
  .kpi-grid {
    grid-template-columns: repeat(2, 1fr);
  }
  .row-two {
    grid-template-columns: 1fr;
  }
}

@media (max-width: 767px) {
  .kpi-grid {
    grid-template-columns: 1fr;
  }
}

@media (max-width: 640px) {
  .trend-bars {
    gap: 1px;
  }
  .trend-bar-label {
    font-size: 9px;
  }
}
</style>
