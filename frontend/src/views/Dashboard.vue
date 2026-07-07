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
  RefreshRight,
} from '@element-plus/icons-vue'
import { ElNotification } from 'element-plus'
import { createChart, LineSeries, LineStyle, CrosshairMode, type IChartApi, type ISeriesApi } from 'lightweight-charts'
import { useDashboard } from '../composables/useDashboard'
import KpiCard from '../components/Dashboard/KpiCard.vue'
import StatusBadge from '../components/Dashboard/StatusBadge.vue'
import CardPanel from '../components/common/CardPanel.vue'
import DataTable from '../components/common/DataTable.vue'
import PageHeader from '../components/common/PageHeader.vue'
import type { KpiData, TrendPoint, SystemHealth, RecentReview } from '../types/dashboard'

const router = useRouter()
const dashboard = useDashboard()

// Loading & refresh state
const loading = dashboard.loading
const isRefreshing = ref(false)
const lastUpdated = ref<string | null>(null)

// Data refs (computed from composable)
const kpis = computed<KpiData | null>(() => dashboard.data.value?.kpis ?? null)
const trend = computed<TrendPoint[]>(() => dashboard.data.value?.trend ?? [])
const health = computed<SystemHealth | null>(() => dashboard.data.value?.health ?? null)
const recentReviews = computed<RecentReview[]>(() => dashboard.data.value?.recentReviews ?? [])

// Chart refs
const chartContainer = ref<HTMLElement | null>(null)
let chart: IChartApi | null = null
let lineSeries: ISeriesApi<'Line'> | null = null

// Auto-refresh timer
let autoRefreshTimer: ReturnType<typeof setInterval> | null = null

// ─── Error Handling ─────────────────────────────────

watch(() => dashboard.error.value, (err) => {
  if (err) {
    ElNotification({
      title: 'Error',
      message: err,
      type: 'error',
      duration: 5000,
    })
  }
})

// ─── Data Fetching ──────────────────────────────────

async function refreshData() {
  await dashboard.refresh()
  lastUpdated.value = new Date().toISOString()
}

async function onRefresh() {
  if (isRefreshing.value) return
  isRefreshing.value = true
  try {
    await refreshData()
    ElNotification({
      title: 'Success',
      message: 'Dashboard refreshed',
      type: 'success',
      duration: 2000,
    })
  } catch (e) {
    ElNotification({
      title: 'Error',
      message: 'Failed to refresh dashboard',
      type: 'error',
      duration: 5000,
    })
  } finally {
    isRefreshing.value = false
  }
}

// ─── Formatters ─────────────────────────────────────

function formatDuration(ms: number): string {
  const mins = Math.floor(ms / 60000)
  const secs = Math.floor((ms % 60000) / 1000)
  return `${mins}m ${secs.toString().padStart(2, '0')}s`
}

function timeAgo(iso: string): string {
  const diff = Date.now() - new Date(iso).getTime()
  const mins = Math.floor(diff / 60000)
  if (mins < 1) return 'just now'
  if (mins < 60) return `${mins} min ago`
  const hrs = Math.floor(mins / 60)
  if (hrs < 24) return `${hrs}h ago`
  return `${Math.floor(hrs / 24)}d ago`
}

function formatTime(iso: string): string {
  return new Date(iso).toLocaleString('en-US', {
    month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit',
  })
}

// ─── Lightweight Charts ───────────────────────────────

function initChart() {
  if (!chartContainer.value) return
  if (chart) {
    chart.remove()
    chart = null
    lineSeries = null
  }

  chart = createChart(chartContainer.value, {
    layout: {
      background: { color: 'transparent' },
      textColor: 'var(--text-secondary)',
    },
    grid: {
      vertLines: { color: 'var(--border-color)', style: LineStyle.SparseDotted },
      horzLines: { color: 'var(--border-color)', style: LineStyle.SparseDotted },
    },
    crosshair: { mode: CrosshairMode.Magnet },
    rightPriceScale: { borderColor: 'var(--border-color)' },
    timeScale: { borderColor: 'var(--border-color)', timeVisible: true },
    handleScroll: false,
    handleScale: false,
    width: chartContainer.value.clientWidth,
    height: 280,
  })

  lineSeries = chart.addSeries(LineSeries, {
    color: 'var(--brand)',
    lineWidth: 2,
    crosshairMarkerVisible: true,
    crosshairMarkerRadius: 4,
    crosshairMarkerBorderColor: 'var(--brand)',
    crosshairMarkerBackgroundColor: 'var(--bg-primary)',
  })

  updateChartData()

  const resizeObserver = new ResizeObserver(() => {
    if (chart && chartContainer.value) {
      chart.applyOptions({ width: chartContainer.value.clientWidth, height: 280 })
    }
  })
  resizeObserver.observe(chartContainer.value)
}

function updateChartData() {
  if (!lineSeries || !trend.value.length) return
  const data: any[] = trend.value.map(p => ({
    time: p.time,
    value: p.value,
  }))
  lineSeries.setData(data)
}

watch(() => trend.value, () => {
  nextTick(() => {
    if (!chart) initChart()
    else updateChartData()
  })
}, { deep: true })

// ─── Table Helpers ──────────────────────────────────

function statusToBadgeStatus(status: RecentReview['status']) {
  switch (status) {
    case 'success': return 'success'
    case 'failed': return 'error'
    case 'running': return 'running'
    case 'queued': return 'queued'
    default: return 'offline'
  }
}

function statusLabel(status: RecentReview['status']): string {
  switch (status) {
    case 'success': return 'Completed'
    case 'failed': return 'Failed'
    case 'running': return 'In Progress'
    case 'queued': return 'Queued'
  }
}

function onRowClick(row: RecentReview) {
  router.push({ path: '/history', query: { reviewId: row.id } })
}

// ─── Lifecycle ──────────────────────────────────────

onMounted(() => {
  refreshData()
  autoRefreshTimer = setInterval(() => {
    refreshData()
  }, 60000)
})

onUnmounted(() => {
  if (autoRefreshTimer) clearInterval(autoRefreshTimer)
  if (chart) {
    chart.remove()
    chart = null
  }
})
</script>

<template>
  <div class="dashboard-page">
    <!-- Page Header -->
    <PageHeader title="Dashboard" subtitle="System overview and recent activity">
      <template #actions>
        <span v-if="lastUpdated" class="last-updated">
          Updated {{ formatTime(lastUpdated) }}
        </span>
        <el-button
          :icon="Refresh"
          :loading="isRefreshing"
          size="small"
          aria-label="Refresh dashboard"
          @click="onRefresh"
        >
          Refresh
        </el-button>
      </template>
    </PageHeader>

    <!-- Row 1: KPI Cards -->
    <div class="kpi-grid">
      <template v-if="loading">
        <el-skeleton v-for="i in 4" :key="i" animated class="kpi-skeleton">
          <template #template>
            <el-skeleton-item variant="circle" style="width: 40px; height: 40px; margin-bottom: 12px;" />
            <el-skeleton-item variant="text" style="width: 60%; height: 20px; margin-bottom: 8px;" />
            <el-skeleton-item variant="text" style="width: 40%; height: 14px;" />
          </template>
        </el-skeleton>
      </template>
      <template v-else-if="kpis">
        <KpiCard
          label="Reviews This Week"
          :value="kpis.reviewsThisWeek"
          format="number"
          :icon="Document"
          :trend="kpis.reviewsTrend"
          trend-label="vs last week"
          style="animation-delay: 0ms"
        />
        <KpiCard
          label="Active Queue"
          :value="kpis.activeQueue"
          format="number"
          :icon="Refresh"
          style="animation-delay: 50ms"
        />
        <KpiCard
          label="Success Rate"
          :value="kpis.successRate"
          format="percent"
          :icon="Check"
          :trend="kpis.successTrend"
          trend-label="vs yesterday"
          style="animation-delay: 100ms"
        />
        <KpiCard
          label="Avg Duration"
          :value="kpis.avgDurationMs"
          format="duration"
          :icon="Timer"
          :trend="kpis.durationTrend"
          trend-label="vs last week"
          style="animation-delay: 150ms"
        />
      </template>
    </div>

    <!-- Row 2: Trend + Health -->
    <div class="row-two">
      <!-- 24h Activity Trend -->
      <CardPanel :body-style="{ padding: '0' }">
        <template #header>
          <div class="card-header">
            <div class="card-header-left">
              <el-icon :size="18"><TrendCharts /></el-icon>
              <span>24h Activity Trend</span>
            </div>
          </div>
        </template>
        <div class="trend-body">
          <el-skeleton v-if="loading" :rows="5" animated />
          <template v-else-if="trend.length > 0">
            <div ref="chartContainer" class="chart-container" />
            <div class="trend-summary">
              <span class="trend-total">Total: {{ trend.reduce((a, b) => a + b.value, 0) }} reviews</span>
            </div>
          </template>
          <div v-else class="trend-empty">
            <el-icon :size="32"><InfoFilled /></el-icon>
            <p>No activity in the last 24 hours</p>
          </div>
        </div>
      </CardPanel>

      <!-- System Health -->
      <CardPanel :body-style="{ padding: '0' }">
        <template #header>
          <div class="card-header">
            <div class="card-header-left">
              <el-icon :size="18"><FirstAidKit /></el-icon>
              <span>System Health</span>
            </div>
            <el-button
              :icon="RefreshRight"
              size="small"
              text
              aria-label="Refresh health data"
              @click="onRefresh"
            />
          </div>
        </template>
        <div class="health-body">
          <el-skeleton v-if="loading" :rows="6" animated />
          <template v-else-if="health">
            <!-- Integrations -->
            <div class="health-section">
              <div class="health-section-title">Integration Status</div>
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
                  <span v-if="item.latencyMs" class="health-latency">{{ item.latencyMs }}ms</span>
                </div>
              </div>
            </div>

            <!-- LLM Providers -->
            <div class="health-section">
              <div class="health-section-title">LLM Providers</div>
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
                  <span v-if="item.latencyMs" class="health-latency">{{ item.latencyMs }}ms</span>
                  <span v-else-if="item.message" class="health-latency">{{ item.message }}</span>
                </div>
              </div>
            </div>

            <!-- Overall -->
            <div class="health-overall">
              <StatusBadge :status="health.overall" size="large" />
              <span class="health-overall-text">
                {{ health.overall === 'success' ? 'All Systems Operational' : health.overall === 'warning' ? 'Some Systems Degraded' : 'System Errors Detected' }}
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
            <span>Recent Reviews</span>
          </div>
          <router-link to="/history" class="view-all-link">
            View All <el-icon :size="12"><ArrowRight /></el-icon>
          </router-link>
        </div>
      </template>
      <div class="recent-body">
        <el-skeleton v-if="loading" :rows="5" animated />
        <template v-else-if="recentReviews.length > 0">
          <div class="table-wrapper">
            <DataTable :data="recentReviews" @row-click="onRowClick">
              <el-table-column label="MR Title" min-width="200">
                <template #default="{ row }">
                  <div class="mr-title-cell">
                    <span class="mr-title-text">{{ row.mrTitle }}</span>
                    <el-tag size="small" type="info" effect="dark">{{ row.project }}</el-tag>
                  </div>
                </template>
              </el-table-column>

              <el-table-column label="Author" width="140">
                <template #default="{ row }">
                  <div class="author-cell">
                    <div class="author-avatar">{{ row.author.name.charAt(0) }}</div>
                    <span>{{ row.author.name }}</span>
                  </div>
                </template>
              </el-table-column>

              <el-table-column label="Status" width="100">
                <template #default="{ row }">
                  <StatusBadge :status="statusToBadgeStatus(row.status)" :show-text="false" size="small" />
                  <span style="margin-left: 6px; font-size: 12px; color: var(--text-primary);">{{ statusLabel(row.status) }}</span>
                </template>
              </el-table-column>

              <el-table-column label="Duration" width="100">
                <template #default="{ row }">
                  <span class="mono-text">{{ formatDuration(row.durationMs) }}</span>
                </template>
              </el-table-column>

              <el-table-column label="Time" width="160">
                <template #default="{ row }">
                  <el-tooltip :content="formatTime(row.createdAt)" placement="top" effect="dark">
                    <span class="mono-text">{{ timeAgo(row.createdAt) }}</span>
                  </el-tooltip>
                </template>
              </el-table-column>
            </DataTable>
          </div>
        </template>
        <div v-else class="recent-empty">
          <el-icon :size="32"><InfoFilled /></el-icon>
          <p>No recent reviews</p>
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

.last-updated {
  font-size: 12px;
  color: var(--text-secondary);
  font-family: var(--font-mono);
}

/* KPI Cards */
.kpi-grid {
  display: grid;
  grid-template-columns: repeat(4, 1fr);
  gap: 16px;
  margin-bottom: 24px;
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
  grid-template-columns: 70% 30%;
  gap: 16px;
  margin-bottom: 24px;
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
  gap: 8px;
  font-weight: 600;
  font-size: 14px;
  color: var(--text-primary);
}

.view-all-link {
  display: flex;
  align-items: center;
  gap: 4px;
  font-size: 12px;
  font-weight: 500;
  color: var(--brand);
}

.view-all-link:hover {
  color: var(--brand-hover);
}

/* Trend Chart */
.trend-body {
  padding: 16px 20px 20px;
}

.chart-container {
  height: 280px;
  width: 100%;
}

.trend-summary {
  margin-top: 12px;
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
  gap: 8px;
}

.trend-empty p, .recent-empty p {
  margin: 0;
  font-size: 14px;
}

/* Health Card */
.health-body {
  padding: 12px 20px 16px;
}

.health-section {
  margin-bottom: 16px;
}

.health-section-title {
  font-size: 11px;
  font-weight: 600;
  text-transform: uppercase;
  letter-spacing: 0.5px;
  color: var(--text-secondary);
  margin-bottom: 4px;
  padding-left: 4px;
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
  gap: 8px;
  padding: 12px 0;
  border-top: 1px solid var(--border-color);
  margin-top: 4px;
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
}

.mr-title-cell {
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.mr-title-text {
  font-size: 13px;
  color: var(--text-primary);
  font-weight: 500;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  max-width: 100%;
}

.author-cell {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 13px;
  color: var(--text-primary);
}

.author-avatar {
  width: 24px;
  height: 24px;
  border-radius: 50%;
  background: var(--brand);
  color: #fff;
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
    grid-template-columns: 60% 40%;
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
