<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted, reactive } from 'vue'
import { ElMessageBox, ElNotification } from 'element-plus'
import type { QueueStats, QueueTask } from '../types/queue'
import StatsCard from '../components/QueueMonitor/StatsCard.vue'
import TaskCard from '../components/QueueMonitor/TaskCard.vue'

// --- Loading state ---
const loading = ref(true)

// --- Queue state ---
const tasks = ref<QueueTask[]>([])
const isPaused = ref(false)
const sseConnected = ref(false)

// --- Section shown tracking ---
const sectionShown = reactive({
  active: false,
  queued: false,
  failed: false,
})

// --- Computed stats ---
const stats = computed<QueueStats>(() => {
  const active = activeTasks.value.length
  const queued = queuedTasks.value.length
  const failed = failedTasks.value.length
  return {
    active,
    queued,
    failed,
    totalDepth: active + queued,
    maxConcurrent: 8,
    queueCapacity: 20,
    failedLast24h: failed + 3,
    totalLast24h: active + queued + failed + 37,
  }
})

// --- Computed task lists ---
const activeTasks = computed(() => tasks.value.filter(t => t.status === 'running'))
const queuedTasks = computed(() => tasks.value.filter(t => t.status === 'queued'))
const failedTasks = computed(() => tasks.value.filter(t => t.status === 'failed'))
const allTasks = computed(() => tasks.value)

// --- Update section shown tracking ---
const updateSectionShown = () => {
  if (activeTasks.value.length > 0) sectionShown.active = true
  if (queuedTasks.value.length > 0) sectionShown.queued = true
  if (failedTasks.value.length > 0) sectionShown.failed = true
}

// --- Mock data generation ---
const generateMockTasks = (): QueueTask[] => {
  const now = Date.now()
  return [
    {
      id: 'task-001',
      mrTitle: 'feat: add authentication middleware',
      project: 'backend',
      repository: 'api-gateway',
      status: 'running',
      progress: 67,
      expertName: 'Security',
      elapsedMs: 150000,
      createdAt: new Date(now - 200000).toISOString(),
      startedAt: new Date(now - 150000).toISOString(),
    },
    {
      id: 'task-002',
      mrTitle: 'fix: resolve memory leak in worker pool',
      project: 'backend',
      repository: 'review-engine',
      status: 'running',
      progress: 34,
      expertName: 'Performance',
      elapsedMs: 45000,
      createdAt: new Date(now - 120000).toISOString(),
      startedAt: new Date(now - 45000).toISOString(),
    },
    {
      id: 'task-003',
      mrTitle: 'refactor: update UI components to composition API',
      project: 'frontend',
      repository: 'dashboard',
      status: 'running',
      progress: 89,
      expertName: 'Code Quality',
      elapsedMs: 312000,
      createdAt: new Date(now - 400000).toISOString(),
      startedAt: new Date(now - 312000).toISOString(),
    },
    {
      id: 'task-004',
      mrTitle: 'docs: update API documentation for v2 endpoints',
      project: 'backend',
      repository: 'api-gateway',
      status: 'running',
      progress: 12,
      expertName: 'Documentation',
      elapsedMs: 15000,
      createdAt: new Date(now - 60000).toISOString(),
      startedAt: new Date(now - 15000).toISOString(),
    },
    {
      id: 'task-005',
      mrTitle: 'feat: implement Redis caching layer',
      project: 'backend',
      repository: 'api-gateway',
      status: 'queued',
      progress: 0,
      expertName: 'Performance',
      elapsedMs: 0,
      createdAt: new Date(now - 80000).toISOString(),
    },
    {
      id: 'task-006',
      mrTitle: 'test: add integration tests for review pipeline',
      project: 'backend',
      repository: 'review-engine',
      status: 'queued',
      progress: 0,
      expertName: 'Testing',
      elapsedMs: 0,
      createdAt: new Date(now - 70000).toISOString(),
    },
    {
      id: 'task-007',
      mrTitle: 'chore: update frontend dependencies to latest',
      project: 'frontend',
      repository: 'dashboard',
      status: 'queued',
      progress: 0,
      expertName: 'Maintenance',
      elapsedMs: 0,
      createdAt: new Date(now - 50000).toISOString(),
    },
    {
      id: 'task-008',
      mrTitle: 'fix: database connection timeout under load',
      project: 'backend',
      repository: 'review-engine',
      status: 'failed',
      progress: 0,
      expertName: 'Reliability',
      elapsedMs: 30000,
      createdAt: new Date(now - 500000).toISOString(),
      startedAt: new Date(now - 470000).toISOString(),
      errorMessage: 'Connection refused after 30s timeout',
    },
    {
      id: 'task-009',
      mrTitle: 'feat: add OAuth2 provider support',
      project: 'backend',
      repository: 'api-gateway',
      status: 'failed',
      progress: 0,
      expertName: 'Security',
      elapsedMs: 45000,
      createdAt: new Date(now - 600000).toISOString(),
      startedAt: new Date(now - 555000).toISOString(),
      errorMessage: 'Invalid client configuration: redirect_uri mismatch',
    },
  ]
}

// --- Load queue data ---
const loadQueueData = async () => {
  loading.value = true
  // Simulate API delay
  await new Promise(resolve => setTimeout(resolve, 800))
  tasks.value = generateMockTasks()
  updateSectionShown()
  loading.value = false
  sseConnected.value = true
}

// --- Mock SSE progress updates ---
let sseInterval: ReturnType<typeof setInterval> | null = null

const startMockSse = () => {
  sseInterval = setInterval(() => {
    tasks.value.forEach(task => {
      if (task.status === 'running' && task.progress < 100) {
        const increment = Math.floor(Math.random() * 4) + 1
        task.progress = Math.min(task.progress + increment, 100)
        if (task.progress === 100) {
          task.status = 'completed'
          task.elapsedMs = task.startedAt
            ? Date.now() - new Date(task.startedAt).getTime()
            : task.elapsedMs
          // Auto-remove completed after 5 seconds
          setTimeout(() => {
            const idx = tasks.value.findIndex(t => t.id === task.id)
            if (idx !== -1) {
              tasks.value.splice(idx, 1)
              updateSectionShown()
            }
          }, 5000)
        }
      }
    })
  }, 2000)
}

// --- Pause / Resume ---
const togglePause = () => {
  isPaused.value = !isPaused.value
  ElNotification({
    type: isPaused.value ? 'warning' : 'success',
    message: isPaused.value ? 'Queue paused' : 'Queue resumed',
    duration: 3000,
  })
}

// --- Cancel all failed ---
const handleCancelAllFailed = async () => {
  const count = failedTasks.value.length
  if (count === 0) return
  try {
    await ElMessageBox.confirm(
      `Cancel all ${count} failed tasks?`,
      'Confirm',
      {
        confirmButtonText: 'Cancel All',
        cancelButtonText: 'Keep',
        type: 'warning',
      }
    )
    tasks.value = tasks.value.filter(t => t.status !== 'failed')
    updateSectionShown()
    ElNotification({
      type: 'success',
      message: 'All failed tasks cancelled',
      duration: 3000,
    })
  } catch {
    // User cancelled the dialog
  }
}

// --- Task actions ---
const handleCancel = async (taskId: string) => {
  const task = tasks.value.find(t => t.id === taskId)
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
    const idx = tasks.value.findIndex(t => t.id === taskId)
    if (idx !== -1) {
      tasks.value.splice(idx, 1)
      updateSectionShown()
    }
    ElNotification({
      type: 'success',
      message: 'Task cancelled',
      duration: 3000,
    })
  } catch {
    // User cancelled
  }
}

const handleRetry = (taskId: string) => {
  const task = tasks.value.find(t => t.id === taskId)
  if (!task) return
  task.status = 'queued'
  task.progress = 0
  task.errorMessage = undefined
  task.elapsedMs = 0
  task.startedAt = undefined
  updateSectionShown()
  ElNotification({
    type: 'success',
    message: 'Task queued for retry',
    duration: 3000,
  })
}

const handleViewLogs = (taskId: string) => {
  ElNotification({
    type: 'info',
    message: `View logs for task ${taskId}`,
    duration: 3000,
  })
}

// --- Lifecycle ---
onMounted(() => {
  loadQueueData().then(() => {
    startMockSse()
  })
})

onUnmounted(() => {
  if (sseInterval) {
    clearInterval(sseInterval)
  }
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

      <!-- SSE Connection Status -->
      <div class="connection-status">
        <el-tag :type="sseConnected ? 'success' : 'info'" size="small" effect="plain">
          <el-icon v-if="sseConnected" class="status-icon"><Loading /></el-icon>
          <span>{{ sseConnected ? 'Live Updates' : 'Connecting...' }}</span>
        </el-tag>
      </div>

      <!-- Active Tasks -->
      <div
        v-if="activeTasks.length > 0 || sectionShown.active"
        class="task-section"
      >
        <div class="section-header">
          <div class="section-title">
            <span>Active Tasks</span>
            <el-badge :value="activeTasks.length" type="primary" />
          </div>
        </div>
        <div v-if="activeTasks.length === 0" class="section-empty">
          <el-empty description="No active tasks">
            <template #image>
              <el-icon :size="48" color="var(--success)"><Check /></el-icon>
            </template>
          </el-empty>
        </div>
        <TransitionGroup v-else name="task" tag="div" class="task-grid">
          <TaskCard
            v-for="task in activeTasks"
            :key="task.id"
            :task="task"
            :is-paused="isPaused"
            @cancel="handleCancel"
            @retry="handleRetry"
            @view-logs="handleViewLogs"
          />
        </TransitionGroup>
      </div>

      <!-- Queued Tasks -->
      <div
        v-if="queuedTasks.length > 0 || sectionShown.queued"
        class="task-section"
      >
        <div class="section-header">
          <div class="section-title">
            <span>Queued Tasks</span>
            <el-badge :value="queuedTasks.length" type="info" />
          </div>
        </div>
        <div v-if="queuedTasks.length === 0" class="section-empty">
          <el-empty description="Queue is empty">
            <template #image>
              <el-icon :size="48" color="var(--info)"><InfoFilled /></el-icon>
            </template>
          </el-empty>
        </div>
        <TransitionGroup v-else name="task" tag="div" class="task-grid">
          <TaskCard
            v-for="task in queuedTasks"
            :key="task.id"
            :task="task"
            :is-paused="isPaused"
            @cancel="handleCancel"
            @retry="handleRetry"
            @view-logs="handleViewLogs"
          />
        </TransitionGroup>
      </div>

      <!-- Failed Tasks -->
      <div
        v-if="failedTasks.length > 0 || sectionShown.failed"
        class="task-section"
      >
        <div class="section-header">
          <div class="section-title">
            <span>Failed Tasks</span>
            <el-badge :value="failedTasks.length" type="danger" />
          </div>
        </div>
        <div v-if="failedTasks.length === 0" class="section-empty">
          <el-empty description="No failed tasks">
            <template #image>
              <el-icon :size="48" color="var(--success)"><Check /></el-icon>
            </template>
          </el-empty>
        </div>
        <TransitionGroup v-else name="task" tag="div" class="task-grid">
          <TaskCard
            v-for="task in failedTasks"
            :key="task.id"
            :task="task"
            :is-paused="isPaused"
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
  align-items: flex-start;
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
  font-size: 20px;
  font-weight: 600;
  color: var(--text-primary);
  margin: 0 0 4px 0;
}

.page-subtitle {
  font-size: 13px;
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

/* Connection Status */
.connection-status {
  margin-bottom: 16px;
  display: flex;
  justify-content: flex-end;
}

.status-icon {
  animation: spin 1s linear infinite;
  margin-right: 4px;
}

@keyframes spin {
  from { transform: rotate(0deg); }
  to { transform: rotate(360deg); }
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
