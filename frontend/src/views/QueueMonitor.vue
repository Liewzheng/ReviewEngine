<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted, watch } from 'vue'
import {
  Loading,
  Collection,
  Warning,
  DataLine,
  VideoPlay,
  VideoPause,
  Delete,
  Refresh,
  InfoFilled,
} from '@element-plus/icons-vue'
import { ElMessageBox, ElNotification } from 'element-plus'
import { useI18n } from 'vue-i18n'
import type { QueueStats, QueueTask } from '../types/queue'
import StatsCard from '../components/QueueMonitor/StatsCard.vue'
import TaskCard from '../components/QueueMonitor/TaskCard.vue'
import { useQueue } from '../composables/useQueue'

const { t } = useI18n()

function notifyError(e: unknown, fallback: string): void {
  ElNotification({
    type: 'error',
    message: e instanceof Error ? e.message : fallback,
    duration: 5000,
  })
}

// --- Composable ---
const queue = useQueue()

// Destructure reactive state for Vue template auto-unwrapping
const isPaused = queue.isPaused
const loading = queue.loading

// --- Local UI state ---
const sseConnected = ref(false)
const recentlyUpdated = ref<string[]>([])
const isRefreshing = ref(false)
let refreshInterval: ReturnType<typeof setInterval> | null = null

// --- Computed stats with fallback ---
const stats = computed<QueueStats>(() => queue.stats.value ?? {
  active: 0,
  queued: 0,
  failed: 0,
  totalDepth: 0,
  maxConcurrent: 8,
  queueCapacity: 20,
  failedLast24h: 0,
  totalLast24h: 0,
  isPaused: false,
})

// --- Computed task lists ---
const activeTasks = computed(() => queue.items.value.filter((t: QueueTask) => t.status === 'running'))
const queuedTasks = computed(() => queue.items.value.filter((t: QueueTask) => t.status === 'queued'))
const failedTasks = computed(() => queue.items.value.filter((t: QueueTask) => t.status === 'failed'))
const cancelledTasks = computed(() => queue.items.value.filter((t: QueueTask) => t.status === 'cancelled'))
const completedTasks = computed(() => queue.items.value.filter((t: QueueTask) => t.status === 'completed'))
// The backend returns every status (including `completed`) when no status
// filter is passed. Completed tasks are rendered in their own section below,
// so the "no tasks" placeholder must cover the full list — otherwise a
// completed-only result suppresses the placeholder while the four active
// sections stay empty, leaving a blank page.
const hasAnyTasks = computed(() => queue.items.value.length > 0)

// --- Load queue data ---
const loadQueueData = async () => {
  await queue.fetchStats()
  await queue.fetchTasks()
  sseConnected.value = true
}

// --- Auto refresh ---
const startAutoRefresh = () => {
  stopAutoRefresh()
  refreshInterval = setInterval(async () => {
    if (isRefreshing.value) return
    isRefreshing.value = true
    try {
      await Promise.all([queue.fetchStats(), queue.fetchTasks()])
    } finally {
      isRefreshing.value = false
    }
  }, 3000)
}

const stopAutoRefresh = () => {
  if (refreshInterval) {
    clearInterval(refreshInterval)
    refreshInterval = null
  }
}

// --- Pause / Resume ---
const togglePause = async () => {
  try {
    if (queue.isPaused.value) {
      await queue.resume()
      ElNotification({
        type: 'success',
        message: t('queue.resumed'),
        duration: 3000,
      })
    } else {
      await queue.pause()
      ElNotification({
        type: 'warning',
        message: t('queue.paused'),
        duration: 3000,
      })
    }
  } catch (e) {
    notifyError(e, t('queue.errors.toggle'))
  }
}

// --- Max concurrent ---
const maxConcurrentInput = ref(8)
let maxConcurrentTimer: ReturnType<typeof setTimeout> | null = null

watch(() => stats.value.maxConcurrent, (val) => {
  maxConcurrentInput.value = val
}, { immediate: true })

// Element Plus emits `change` only when the value is committed via Enter or
// blur — the +/- stepper buttons only emit `update:model-value`, so a
// `@change`-only binding leaves the stepper dead (value looks unchanged, no
// request). Commit on any model update through a short debounce, and let
// Enter/blur commit immediately (cancelling any pending debounce so each
// logical action sends exactly one request).
async function commitMaxConcurrent() {
  if (maxConcurrentTimer) {
    clearTimeout(maxConcurrentTimer)
    maxConcurrentTimer = null
  }
  const value = Math.max(1, Math.min(64, maxConcurrentInput.value))
  maxConcurrentInput.value = value
  try {
    await queue.updateMaxConcurrent(value)
    ElNotification({
      type: 'success',
      message: t('queue.maxConcurrentSet', { n: value }),
      duration: 3000,
    })
  } catch (e) {
    notifyError(e, t('queue.errors.maxConcurrent'))
  }
}

const handleMaxConcurrentChange = () => {
  commitMaxConcurrent()
}

function onMaxConcurrentModelUpdate() {
  if (maxConcurrentTimer) clearTimeout(maxConcurrentTimer)
  maxConcurrentTimer = setTimeout(commitMaxConcurrent, 400)
}

// --- Cancel all failed ---
const handleCancelAllFailed = async () => {
  if (failedTasks.value.length === 0) {
    ElNotification({ type: 'info', message: t('queue.noFailedToCancel'), duration: 3000 })
    return
  }
  try {
    await ElMessageBox.confirm(
      t('queue.cancelAllConfirm', { count: failedTasks.value.length }),
      t('queue.confirmTitle'),
      {
        confirmButtonText: t('queue.cancelAllBtn'),
        cancelButtonText: t('common.keep'),
        type: 'warning',
      }
    )
    const results = await Promise.allSettled(failedTasks.value.map((task) => queue.cancel(task.id)))
    const succeeded: string[] = []
    const failedIds: string[] = []
    results.forEach((result, index) => {
      if (result.status === 'fulfilled') {
        succeeded.push(failedTasks.value[index].id)
      } else {
        failedIds.push(failedTasks.value[index].id)
      }
    })
    await queue.fetchTasks()
    await queue.fetchStats()
    const type = failedIds.length === 0 ? 'success' : succeeded.length === 0 ? 'error' : 'warning'
    ElNotification({
      type,
      message: t('queue.cancelAllResult', { succeeded: succeeded.length, failed: failedIds.length }),
      duration: 5000,
    })
  } catch (e) {
    if (e === 'cancel' || e === 'close') return
    notifyError(e, t('queue.errors.cancelAllFailed'))
  }
}

// --- Task actions ---
const handleCancel = async (taskId: string) => {
  const task = queue.items.value.find((item: QueueTask) => item.id === taskId)
  if (!task) return
  try {
    await ElMessageBox.confirm(
      t('queue.cancelConfirm', { title: task.mrTitle }),
      t('queue.cancelTitle'),
      {
        confirmButtonText: t('queue.cancelReviewBtn'),
        cancelButtonText: t('common.keep'),
        type: 'warning',
      }
    )
    await queue.cancel(taskId)
    await queue.fetchTasks()
    await queue.fetchStats()
    ElNotification({
      type: 'success',
      message: t('queue.taskCancelled'),
      duration: 3000,
    })
  } catch (e) {
    if (e === 'cancel' || e === 'close') return
    notifyError(e, t('queue.errors.cancel'))
  }
}

const handleRetry = async (taskId: string) => {
  try {
    await queue.retry(taskId)
    ElNotification({
      type: 'success',
      message: t('queue.taskQueuedForRetry'),
      duration: 3000,
    })
  } catch (e) {
    notifyError(e, t('queue.errors.retry'))
  }
}

const handleViewLogs = (taskId: string) => {
  ElNotification({
    type: 'info',
    message: t('queue.viewLogs', { id: taskId }),
    duration: 3000,
  })
}

// --- Error handling ---
watch(() => queue.error.value, (err) => {
  if (err) {
    ElNotification({
      type: 'error',
      message: err,
      duration: 5000,
    })
  }
})

// --- Lifecycle ---
onMounted(() => {
  loadQueueData().then(() => {
    startAutoRefresh()
  })
})

onUnmounted(() => {
  stopAutoRefresh()
})
</script>

<template>
  <div class="queue-page">
    <!-- Page Header -->
    <div class="page-header">
      <div class="page-header-left">
        <h2 class="page-title">{{ $t('queue.title') }}</h2>
        <p class="page-subtitle">{{ $t('queue.subtitle') }}</p>
      </div>
      <div class="page-header-right">
        <el-button
          :type="isPaused ? 'success' : 'warning'"
          @click="togglePause"
        >
          <el-icon class="btn-icon">
            <component :is="isPaused ? VideoPlay : VideoPause" />
          </el-icon>
          <span>{{ isPaused ? $t('queue.resumeQueue') : $t('queue.pauseQueue') }}</span>
        </el-button>
        <span class="toolbar-label" :title="$t('queue.maxConcurrentTitle')">{{ $t('queue.maxConcurrent') }}</span>
        <el-input-number
          v-model="maxConcurrentInput"
          :min="1"
          :max="64"
          size="default"
          style="width: 120px"
          :aria-label="$t('queue.maxConcurrent')"
          :title="$t('queue.maxConcurrentTitle')"
          @change="handleMaxConcurrentChange"
          @update:model-value="onMaxConcurrentModelUpdate"
        />
        <!-- Wrapper span: a disabled button swallows pointer events, so the
             "no failed tasks" tooltip needs a hoverable parent. -->
        <el-tooltip :content="$t('queue.noFailedToCancel')" :disabled="failedTasks.length > 0" placement="bottom">
          <span>
            <el-button type="danger" :disabled="failedTasks.length === 0" @click="handleCancelAllFailed">
              <el-icon class="btn-icon"><Delete /></el-icon>
              <span>{{ $t('queue.cancelAllFailed') }}</span>
            </el-button>
          </span>
        </el-tooltip>
        <el-button @click="loadQueueData">
          <el-icon class="btn-icon"><Refresh /></el-icon>
          <span>{{ $t('common.refresh') }}</span>
        </el-button>
      </div>
    </div>

    <!-- Loading Skeleton -->
    <template v-if="loading">
      <div class="stats-skeleton">
        <div v-for="i in 4" :key="`s-${i}`" class="skeleton-item">
          <el-skeleton :rows="2" animated />
        </div>
      </div>
      <div class="tasks-skeleton">
        <div v-for="i in 6" :key="`t-${i}`" class="skeleton-item">
          <el-skeleton :rows="4" animated />
        </div>
      </div>
    </template>

    <!-- Content -->
    <template v-else>
      <!-- Stats Row -->
      <div class="stats-row">
        <StatsCard
          :label="$t('queue.stats.active')"
          :value="stats.active"
          :icon="Loading"
          color="var(--brand)"
          :max="stats.maxConcurrent"
        />
        <StatsCard
          :label="$t('queue.stats.queued')"
          :value="stats.queued"
          :icon="Collection"
          color="var(--info)"
          :max="stats.queueCapacity"
        />
        <StatsCard
          :label="$t('queue.stats.failed')"
          :value="stats.failed"
          :icon="Warning"
          color="var(--error)"
          :max="Math.max(stats.totalLast24h, 1)"
        />
        <StatsCard
          :label="$t('queue.stats.depth')"
          :value="stats.totalDepth"
          :icon="DataLine"
          color="var(--warning)"
          :max="stats.queueCapacity"
        />
      </div>

      <!-- Active Tasks -->
      <div
        v-if="activeTasks.length > 0"
        class="task-section"
      >
        <div class="section-header">
          <div class="section-title">
            <span>{{ $t('queue.sections.active') }}</span>
            <el-badge :value="activeTasks.length" type="primary" />
          </div>
        </div>
        <TransitionGroup name="task" tag="div" class="task-grid">
          <TaskCard
            v-for="task in activeTasks"
            :key="task.id"
            :task="task"
            :is-paused="isPaused"
            :was-updated="recentlyUpdated.includes(task.id)"
            @cancel="handleCancel"
            @retry="handleRetry"
            @view-logs="handleViewLogs"
          />
        </TransitionGroup>
      </div>

      <!-- Queued Tasks -->
      <div
        v-if="queuedTasks.length > 0"
        class="task-section"
      >
        <div class="section-header">
          <div class="section-title">
            <span>{{ $t('queue.sections.queued') }}</span>
            <el-badge :value="queuedTasks.length" type="info" />
          </div>
        </div>
        <TransitionGroup name="task" tag="div" class="task-grid">
          <TaskCard
            v-for="task in queuedTasks"
            :key="task.id"
            :task="task"
            :is-paused="isPaused"
            :was-updated="recentlyUpdated.includes(task.id)"
            @cancel="handleCancel"
            @retry="handleRetry"
            @view-logs="handleViewLogs"
          />
        </TransitionGroup>
      </div>

      <!-- Failed Tasks -->
      <div
        v-if="failedTasks.length > 0"
        class="task-section"
      >
        <div class="section-header">
          <div class="section-title">
            <span>{{ $t('queue.sections.failed') }}</span>
            <el-badge :value="failedTasks.length" type="danger" />
          </div>
        </div>
        <TransitionGroup name="task" tag="div" class="task-grid">
          <TaskCard
            v-for="task in failedTasks"
            :key="task.id"
            :task="task"
            :is-paused="isPaused"
            :was-updated="recentlyUpdated.includes(task.id)"
            @cancel="handleCancel"
            @retry="handleRetry"
            @view-logs="handleViewLogs"
          />
        </TransitionGroup>
      </div>

      <!-- Cancelled Tasks -->
      <div
        v-if="cancelledTasks.length > 0"
        class="task-section"
      >
        <div class="section-header">
          <div class="section-title">
            <span>{{ $t('queue.sections.cancelled') }}</span>
            <el-badge :value="cancelledTasks.length" type="info" />
          </div>
        </div>
        <TransitionGroup name="task" tag="div" class="task-grid">
          <TaskCard
            v-for="task in cancelledTasks"
            :key="task.id"
            :task="task"
            :is-paused="isPaused"
            :was-updated="recentlyUpdated.includes(task.id)"
            @cancel="handleCancel"
            @retry="handleRetry"
            @view-logs="handleViewLogs"
          />
        </TransitionGroup>
      </div>

      <!-- Recently Completed Tasks -->
      <div
        v-if="completedTasks.length > 0"
        class="task-section"
      >
        <div class="section-header">
          <div class="section-title">
            <span>{{ $t('queue.sections.completed') }}</span>
            <el-badge :value="completedTasks.length" type="success" />
          </div>
        </div>
        <TransitionGroup name="task" tag="div" class="task-grid">
          <TaskCard
            v-for="task in completedTasks"
            :key="task.id"
            :task="task"
            :is-paused="isPaused"
            :was-updated="recentlyUpdated.includes(task.id)"
            @cancel="handleCancel"
            @retry="handleRetry"
            @view-logs="handleViewLogs"
          />
        </TransitionGroup>
      </div>

      <!-- Global Empty State -->
      <div v-if="!hasAnyTasks" class="global-empty">
        <el-empty :description="$t('queue.empty')">
          <template #image>
            <el-icon :size="64" color="var(--text-secondary)"><InfoFilled /></el-icon>
          </template>
          <p class="empty-text">{{ $t('queue.emptyHint') }}</p>
        </el-empty>
      </div>
    </template>
  </div>
</template>

<style scoped>
.queue-page {
  padding-bottom: 40px;
  max-width: 1400px;
  margin: 0 auto;
}

/* Page Header */
.page-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  margin-bottom: 24px;
  flex-wrap: wrap;
}

.page-header-left {
  flex: 1;
  min-width: 0;
}

.page-title {
  font-size: 24px;
  font-weight: 600;
  color: var(--text-primary);
  margin: 0 0 4px 0;
}

.page-subtitle {
  font-size: 14px;
  color: var(--text-secondary);
  margin: 0;
}

.page-header-right {
  display: flex;
  gap: 8px;
  flex-wrap: wrap;
  align-items: center;
}

.toolbar-label {
  font-size: 14px;
  color: var(--text-secondary);
  white-space: nowrap;
}

.btn-icon {
  margin-right: 4px;
}

/* Skeleton */
.stats-skeleton {
  display: grid;
  grid-template-columns: repeat(4, 1fr);
  gap: 16px;
  margin-bottom: 24px;
}

.tasks-skeleton {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));
  gap: 16px;
}

.skeleton-item {
  padding: 16px;
  background: var(--bg-card);
  border-radius: var(--radius-md);
  border: 1px solid var(--border-color);
}

/* Stats Row */
.stats-row {
  display: grid;
  grid-template-columns: repeat(4, 1fr);
  gap: 16px;
  margin-bottom: 16px;
}

/* Task Sections */
.task-section {
  margin-top: 24px;
}

.task-section:first-of-type {
  margin-top: 0;
}

.section-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding-bottom: 12px;
  border-bottom: 1px solid var(--border-color);
  margin-bottom: 16px;
}

.section-title {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 16px;
  font-weight: 600;
  color: var(--text-primary);
}

.section-empty {
  padding: 32px 0;
}

/* Task Grid */
.task-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));
  gap: 16px;
}

/* Global Empty */
.global-empty {
  padding: 64px 0;
}

.empty-text {
  color: var(--text-secondary);
  font-size: 14px;
  margin-top: 8px;
}

/* Transitions */
.task-enter-active {
  transition: all 0.25s cubic-bezier(0.4, 0, 0.2, 1);
}

.task-enter-from {
  opacity: 0;
  transform: translateY(12px);
}

.task-leave-active {
  transition: all 0.2s ease;
}

.task-leave-to {
  opacity: 0;
  transform: scale(0.95);
}

.task-move {
  transition: all 0.3s ease;
}

/* Responsive */
@media (min-width: 1024px) and (max-width: 1279px) {
  .task-grid {
    grid-template-columns: repeat(3, 1fr);
  }
}

@media (max-width: 1023px) {
  .task-grid {
    grid-template-columns: repeat(2, 1fr);
  }
}

@media (max-width: 767px) {
  .stats-row,
  .stats-skeleton {
    grid-template-columns: repeat(2, 1fr);
  }

  .task-grid {
    grid-template-columns: 1fr;
  }

  .page-header {
    flex-direction: column;
  }

  .page-header-right {
    width: 100%;
  }

  .page-header-right .el-button {
    flex: 1;
  }
}

@media (max-width: 479px) {
  .stats-row,
  .stats-skeleton {
    grid-template-columns: 1fr;
  }

  .page-header-right {
    flex-direction: column;
  }

  .page-header-right .el-button {
    width: 100%;
    justify-content: center;
  }
}
</style>
