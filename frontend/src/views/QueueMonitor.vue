<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted, watch } from 'vue'
import { ElMessageBox, ElNotification } from 'element-plus'
import type { QueueStats, QueueTask } from '../types/queue'
import StatsCard from '../components/QueueMonitor/StatsCard.vue'
import TaskCard from '../components/QueueMonitor/TaskCard.vue'
import { useQueue } from '../composables/useQueue'

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
const allTasks = computed(() => queue.items.value)

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
        message: 'Queue resumed',
        duration: 3000,
      })
    } else {
      await queue.pause()
      ElNotification({
        type: 'warning',
        message: 'Queue paused',
        duration: 3000,
      })
    }
  } catch (e) {
    ElNotification({
      type: 'error',
      message: e instanceof Error ? e.message : 'Failed to toggle queue',
      duration: 5000,
    })
  }
}

// --- Max concurrent ---
const maxConcurrentInput = ref(8)

watch(() => stats.value.maxConcurrent, (val) => {
  maxConcurrentInput.value = val
}, { immediate: true })

const handleMaxConcurrentChange = async () => {
  const value = Math.max(1, Math.min(64, maxConcurrentInput.value))
  maxConcurrentInput.value = value
  try {
    await queue.updateMaxConcurrent(value)
    ElNotification({
      type: 'success',
      message: `Max concurrent set to ${value}`,
      duration: 3000,
    })
  } catch (e) {
    ElNotification({
      type: 'error',
      message: e instanceof Error ? e.message : 'Failed to update max concurrent',
      duration: 5000,
    })
  }
}

// --- Cancel all failed ---
const handleCancelAllFailed = async () => {
  if (failedTasks.value.length === 0) {
    ElNotification({ type: 'info', message: 'No failed tasks to cancel', duration: 3000 })
    return
  }
  try {
    await ElMessageBox.confirm(
      `Cancel all ${failedTasks.value.length} failed tasks?`,
      'Confirm',
      {
        confirmButtonText: 'Cancel All',
        cancelButtonText: 'Keep',
        type: 'warning',
      }
    )
    const results = await Promise.allSettled(failedTasks.value.map(t => queue.cancel(t.id)))
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
      message: `Cancelled ${succeeded.length} tasks, ${failedIds.length} failed`,
      duration: 5000,
    })
  } catch {
    // User cancelled the dialog
  }
}

// --- Task actions ---
const handleCancel = async (taskId: string) => {
  const task = queue.items.value.find((t: QueueTask) => t.id === taskId)
  if (!task) return
  try {
    await ElMessageBox.confirm(
      `Cancel review for "${task.mrTitle}"? This action cannot be undone.`,
      'Confirm Cancel',
      {
        confirmButtonText: 'Cancel Review',
        cancelButtonText: 'Keep',
        type: 'warning',
      }
    )
    await queue.cancel(taskId)
    await queue.fetchTasks()
    await queue.fetchStats()
    ElNotification({
      type: 'success',
      message: 'Task cancelled',
      duration: 3000,
    })
  } catch {
    // User cancelled
  }
}

const handleRetry = async (taskId: string) => {
  try {
    await queue.retry(taskId)
    ElNotification({
      type: 'success',
      message: 'Task queued for retry',
      duration: 3000,
    })
  } catch (e) {
    ElNotification({
      type: 'error',
      message: e instanceof Error ? e.message : 'Failed to retry task',
      duration: 5000,
    })
  }
}

const handleViewLogs = (taskId: string) => {
  ElNotification({
    type: 'info',
    message: `View logs for task ${taskId}`,
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
        <h2 class="page-title">Queue Monitor</h2>
        <p class="page-subtitle">Real-time review task queue</p>
      </div>
      <div class="page-header-right">
        <el-button
          :type="isPaused ? 'success' : 'warning'"
          @click="togglePause"
        >
          <el-icon class="btn-icon">
            <component :is="isPaused ? 'VideoPlay' : 'VideoPause'" />
          </el-icon>
          <span>{{ isPaused ? 'Resume Queue' : 'Pause Queue' }}</span>
        </el-button>
        <el-input-number
          v-model="maxConcurrentInput"
          :min="1"
          :max="64"
          size="default"
          style="width: 120px"
          @change="handleMaxConcurrentChange"
        />
        <el-button type="danger" @click="handleCancelAllFailed">
          <el-icon class="btn-icon"><Delete /></el-icon>
          <span>Cancel All Failed</span>
        </el-button>
        <el-button @click="loadQueueData">
          <el-icon class="btn-icon"><Refresh /></el-icon>
          <span>Refresh</span>
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
          label="Active Tasks"
          :value="stats.active"
          icon="Loading"
          color="var(--brand)"
          :max="stats.maxConcurrent"
        />
        <StatsCard
          label="Queued Tasks"
          :value="stats.queued"
          icon="Collection"
          color="var(--info)"
          :max="stats.queueCapacity"
        />
        <StatsCard
          label="Failed Tasks"
          :value="stats.failed"
          icon="Warning"
          color="var(--error)"
          :max="Math.max(stats.totalLast24h, 1)"
        />
        <StatsCard
          label="Queue Depth"
          :value="stats.totalDepth"
          icon="DataLine"
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
            <span>Active Tasks</span>
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
            <span>Queued Tasks</span>
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
            <span>Failed Tasks</span>
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

      <!-- Global Empty State -->
      <div v-if="allTasks.length === 0" class="global-empty">
        <el-empty description="No tasks in queue">
          <template #image>
            <el-icon :size="64" color="var(--text-secondary)"><InfoFilled /></el-icon>
          </template>
          <p class="empty-text">The queue is currently empty. New tasks will appear here.</p>
        </el-empty>
      </div>
    </template>
  </div>
</template>

<style scoped>
.queue-page {
  padding-bottom: 40px;
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
