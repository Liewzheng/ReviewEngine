<template>
  <div class="logs-page">
    <!-- Page Header -->
    <div class="page-header">
      <div class="header-title">
        <h2 class="page-title">System Logs</h2>
        <p class="page-subtitle">Live log stream</p>
      </div>
      <div class="header-actions">
        <el-button
          :type="logs.isPaused ? 'warning' : 'default'"
          :icon="logs.isPaused ? VideoPlay : VideoPause"
          @click="togglePause"
        >
          {{ logs.isPaused ? 'Resume' : 'Pause' }}
        </el-button>
        <el-button
          type="primary"
          :icon="Download"
          :loading="downloading"
          @click="downloadLogs"
        >
          Download
        </el-button>
        <el-button
          type="danger"
          :icon="Delete"
          @click="confirmClear"
        >
          Clear
        </el-button>
      </div>
    </div>

    <!-- Toolbar -->
    <div class="toolbar" :class="{ paused: logs.isPaused }">
      <div class="toolbar-row">
        <!-- Level Filter -->
        <div class="filter-group">
          <span class="filter-label">Levels:</span>
          <el-checkbox-group v-model="logs.levels" size="small">
            <el-checkbox label="INFO">
              <span class="level-dot" style="background-color: var(--info)"></span>
              INFO
            </el-checkbox>
            <el-checkbox label="WARN">
              <span class="level-dot" style="background-color: var(--warning)"></span>
              WARN
            </el-checkbox>
            <el-checkbox label="ERROR">
              <span class="level-dot" style="background-color: var(--error)"></span>
              ERROR
            </el-checkbox>
            <el-checkbox label="DEBUG">
              <span class="level-dot" style="background-color: var(--offline)"></span>
              DEBUG
            </el-checkbox>
          </el-checkbox-group>
        </div>

        <!-- Keyword Search -->
        <div class="search-group">
          <el-input
            v-model="searchInput"
            placeholder="Filter logs..."
            clearable
            size="small"
            class="search-input"
          >
            <template #prefix>
              <el-icon><Search /></el-icon>
            </template>
          </el-input>
        </div>
      </div>

      <div class="toolbar-row toolbar-bottom">
        <div class="toolbar-left">
          <!-- Auto-scroll Toggle -->
          <el-switch
            v-model="autoScroll"
            active-text="Auto-scroll"
            class="auto-scroll-switch"
          />

          <!-- Timestamp Format -->
          <div class="format-select">
            <span class="filter-label">Format:</span>
            <el-select v-model="timestampFormat" size="small" style="width: 120px">
              <el-option label="Relative" value="relative" />
              <el-option label="Absolute" value="absolute" />
              <el-option label="ISO" value="iso" />
            </el-select>
          </div>
        </div>

        <div class="toolbar-right">
          <span v-if="logs.isPaused" class="pause-indicator">
            <el-icon><VideoPause /></el-icon>
            Paused
          </span>
          <span class="filter-count">
            Showing {{ filteredLogs.length }} of {{ logItems.length }} logs
          </span>
        </div>
      </div>
    </div>

    <!-- Loading State -->
    <div v-if="logs.loading" class="loading-container">
      <el-skeleton :rows="15" animated />
    </div>

    <!-- Log Terminal -->
    <div v-else ref="terminalRef" class="log-terminal" @scroll="handleScroll">
      <!-- Empty: Cleared -->
      <div v-if="isCleared && logItems.length === 0" class="empty-state">
        <el-empty description="Logs cleared. New entries will appear here.">
          <template #image>
            <el-icon size="48" color="#6b7280"><Check /></el-icon>
          </template>
        </el-empty>
      </div>

      <!-- Empty: No logs yet -->
      <div v-else-if="logItems.length === 0" class="empty-state">
        <el-empty description="Waiting for logs...">
          <template #image>
            <el-icon size="48" color="#6b7280" class="is-loading"><Loading /></el-icon>
          </template>
        </el-empty>
      </div>

      <!-- Empty: All filtered out -->
      <div v-else-if="filteredLogs.length === 0 && logItems.length > 0" class="empty-state">
        <el-empty description="No logs match current filters">
          <template #image>
            <el-icon size="48" color="#6b7280"><InfoFilled /></el-icon>
          </template>
        </el-empty>
      </div>

      <!-- Log Lines -->
      <div v-else class="log-lines">
        <div
          v-for="log in filteredLogs"
          :key="log.id"
          class="log-line"
          :class="{
            'log-error': log.level === 'ERROR',
            'log-warn': log.level === 'WARN',
          }"
        >
          <span class="log-timestamp">{{ formatTimestamp(log.timestamp) }}</span>
          <el-tag
            :type="getLevelTagType(log.level)"
            size="small"
            class="log-level"
            effect="dark"
          >
            {{ log.level }}
          </el-tag>
          <span class="log-message" v-html="highlightMessage(log.message)"></span>
          <span v-if="log.metadata && (log.metadata.durationMs || log.metadata.requestId)" class="log-meta">
            <span v-if="log.metadata.durationMs" class="meta-duration">{{ log.metadata.durationMs }}ms</span>
            <span v-if="log.metadata.requestId" class="meta-request">{{ log.metadata.requestId }}</span>
          </span>
        </div>
      </div>
    </div>

    <!-- Floating New Logs Button -->
    <transition name="slide-up">
      <el-button
        v-if="(newLogCount > 0 && !autoScroll) || logs.isPaused"
        type="primary"
        class="new-logs-btn"
        :icon="ArrowDown"
        @click="scrollToBottom"
      >
        {{ logs.isPaused ? 'Resume' : newLogCount + ' new logs' }}
      </el-button>
    </transition>
  </div>
</template>

<script setup lang="ts">
import { ref, computed, nextTick, watch, onMounted, onUnmounted } from 'vue'
import {
  Search,
  VideoPause,
  VideoPlay,
  Download,
  Delete,
  Loading,
  InfoFilled,
  Check,
  ArrowDown,
} from '@element-plus/icons-vue'
import { ElMessageBox, ElNotification } from 'element-plus'
import type { LogLevel, TimestampFormat } from '../types/logs'
import { useLogs } from '../composables/useLogs'

// ==================== Composable ====================
const logs = useLogs()

// ==================== Local State ====================
const autoScroll = ref(true)
const timestampFormat = ref<TimestampFormat>('relative')
const isCleared = ref(false)
const newLogCount = ref(0)
const downloading = ref(false)
const terminalRef = ref<HTMLElement | null>(null)
const searchInput = ref('')

let keywordDebounceTimer: number | null = null
let newLogDismissTimer: number | null = null

// ==================== Debounce ====================
watch(searchInput, (val) => {
  if (keywordDebounceTimer) window.clearTimeout(keywordDebounceTimer)
  keywordDebounceTimer = window.setTimeout(() => {
    logs.keyword.value = val
  }, 150)
})

// ==================== Computed ====================
const filteredLogs = computed(() => logs.filteredLogs.value)
const logItems = computed(() => logs.logs.value)

// ==================== Formatting ====================
function formatTimestamp(iso: string): string {
  const d = new Date(iso)
  if (timestampFormat.value === 'iso') {
    return d.toISOString()
  }
  if (timestampFormat.value === 'absolute') {
    return d.toLocaleTimeString('en-US', { hour12: false })
  }
  // relative
  const diff = Date.now() - d.getTime()
  const sec = Math.floor(diff / 1000)
  if (sec < 60) return `${sec}s ago`
  if (sec < 3600) return `${Math.floor(sec / 60)}m ago`
  return `${Math.floor(sec / 3600)}h ago`
}

function getLevelTagType(level: LogLevel): 'info' | 'warning' | 'danger' | undefined {
  switch (level) {
    case 'INFO': return 'info'
    case 'WARN': return 'warning'
    case 'ERROR': return 'danger'
    case 'DEBUG': return undefined
    default: return undefined
  }
}

function highlightMessage(msg: string): string {
  let html = escapeHtml(msg)

  const kw = logs.keyword.value.trim()
  if (kw) {
    const re = new RegExp(`(${escapeRegExp(kw)})`, 'gi')
    html = html.replace(re, '<mark>$1</mark>')
  }

  // Linkify review IDs
  html = html.replace(/MR !(\d+)/g, '<a href="#/history?reviewId=$1" class="log-link">MR !$1</a>')

  return html
}

function escapeHtml(s: string): string {
  return s
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
}

function escapeRegExp(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
}

// ==================== Actions ====================
function togglePause() {
  logs.togglePause()
}

// function trimLogs() {
//   if (logs.logs.value.length > 5000) {
//     logs.logs.value = logs.logs.value.slice(-5000)
//   }
// }

function confirmClear() {
  ElMessageBox.confirm(
    'Clear visible logs? This only affects the display, not stored logs.',
    'Clear Logs',
    { confirmButtonText: 'Clear', cancelButtonText: 'Cancel', type: 'warning' }
  ).then(() => {
    logs.clearLogs()
    isCleared.value = true
    newLogCount.value = 0
  }).catch(() => {})
}

async function downloadLogs() {
  downloading.value = true
  try {
    await logs.download()
    ElNotification({
      title: 'Download Started',
      message: 'Your log file is being downloaded.',
      type: 'success',
      duration: 3000,
    })
  } catch {
    // error handled by composable
  } finally {
    downloading.value = false
  }
}

function scrollToBottom() {
  if (logs.isPaused.value) {
    logs.togglePause()
  }
  if (newLogDismissTimer) window.clearTimeout(newLogDismissTimer)
  nextTick(() => {
    if (terminalRef.value) {
      terminalRef.value.scrollTop = terminalRef.value.scrollHeight
    }
    newLogCount.value = 0
  })
}

function handleScroll() {
  if (!terminalRef.value || autoScroll.value) return
  const { scrollTop, scrollHeight, clientHeight } = terminalRef.value
  const atBottom = scrollHeight - scrollTop - clientHeight < 20
  if (atBottom) {
    newLogCount.value = 0
  }
}

// ==================== Error handling ====================
watch(() => logs.error.value, (err) => {
  if (err) {
    ElNotification({
      type: 'error',
      title: 'Log Stream Error',
      message: err,
      duration: 5000,
    })
  }
})

// Watch for new logs to clear isCleared and update newLogCount
watch(() => logs.logs.value.length, (newLength, oldLength) => {
  if (oldLength !== undefined && newLength > oldLength) {
    isCleared.value = false
    if (!autoScroll.value && !logs.isPaused.value) {
      newLogCount.value++
      if (newLogDismissTimer) window.clearTimeout(newLogDismissTimer)
      newLogDismissTimer = window.setTimeout(() => { newLogCount.value = 0 }, 10000)
    }
  }
})

// ==================== Lifecycle ====================
onMounted(() => {
  nextTick(() => {
    if (autoScroll.value) scrollToBottom()
  })
})

// Watch auto-scroll changes
watch(autoScroll, (val) => {
  if (val) scrollToBottom()
})

onUnmounted(() => {
  if (newLogDismissTimer) clearTimeout(newLogDismissTimer)
  if (keywordDebounceTimer) clearTimeout(keywordDebounceTimer)
})
</script>

<style scoped>
.logs-page {
  display: flex;
  flex-direction: column;
  height: 100%;
  gap: 12px;
}

/* Page Header */
.page-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  flex-wrap: wrap;
  gap: 12px;
  padding-bottom: 8px;
  border-bottom: 1px solid var(--border-color);
}

.header-title {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.page-title {
  font-size: 20px;
  font-weight: 600;
  color: var(--text-primary);
  margin: 0;
}

.page-subtitle {
  font-size: 13px;
  color: var(--text-secondary);
  margin: 0;
}

.header-actions {
  display: flex;
  gap: 8px;
}

/* Toolbar */
.toolbar {
  display: flex;
  flex-direction: column;
  gap: 8px;
  padding: 12px 16px;
  background-color: var(--bg-card);
  border: 1px solid var(--border-color);
  border-radius: var(--radius-md);
  transition: background-color 0.3s ease, border-color 0.3s ease;
}

.toolbar.paused {
  background-color: rgba(245, 158, 11, 0.1);
  border-color: var(--warning);
}

.toolbar-row {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 16px;
}

.toolbar-bottom {
  justify-content: space-between;
}

.toolbar-left {
  display: flex;
  align-items: center;
  gap: 16px;
  flex-wrap: wrap;
}

.toolbar-right {
  display: flex;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
}

.filter-group {
  display: flex;
  align-items: center;
  gap: 8px;
}

.filter-label {
  font-size: 13px;
  color: var(--text-secondary);
  white-space: nowrap;
}

.level-dot {
  display: inline-block;
  width: 8px;
  height: 8px;
  border-radius: 50%;
  margin-right: 4px;
  vertical-align: middle;
}

.search-group {
  flex: 1;
  min-width: 200px;
  max-width: 320px;
}

.search-input :deep(.el-input__wrapper) {
  background-color: var(--bg-surface);
}

.format-select {
  display: flex;
  align-items: center;
  gap: 8px;
}

.auto-scroll-switch :deep(.el-switch__label) {
  color: var(--text-secondary);
}

.pause-indicator {
  display: flex;
  align-items: center;
  gap: 4px;
  font-size: 13px;
  color: var(--warning);
  font-weight: 500;
}

.filter-count {
  font-size: 13px;
  color: var(--text-secondary);
}

/* Loading */
.loading-container {
  flex: 1;
  padding: 16px;
  background-color: var(--bg-surface);
  border: 1px solid var(--border-color);
  border-radius: var(--radius-md);
  overflow-y: auto;
}

/* Log Terminal */
.log-terminal {
  flex: 1;
  background-color: #0a0a0a;
  border: 1px solid var(--border-color);
  border-radius: var(--radius-md);
  padding: 16px;
  overflow-y: auto;
  font-family: var(--font-mono);
  font-size: 13px;
  line-height: 1.6;
  min-height: 200px;
  max-height: calc(100vh - 240px);
}

/* Empty States */
.empty-state {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  height: 100%;
  padding: 48px 0;
}

.empty-state .el-empty {
  --el-empty-description-color: #9ca3af;
}

.empty-state .el-empty__image {
  width: auto;
  height: auto;
}

/* Log Lines */
.log-lines {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.log-line {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 3px 6px;
  border-radius: 4px;
  user-select: text;
  animation: fadeIn 0.15s ease;
  transition: background-color 0.1s ease;
  flex-wrap: nowrap;
  font-family: var(--font-mono);
}

@keyframes fadeIn {
  from { opacity: 0; }
  to { opacity: 1; }
}

.log-line:hover {
  background-color: rgba(255, 255, 255, 0.04);
}

.log-line.log-error {
  border-left: 2px solid var(--error);
  padding-left: 6px;
  margin-left: 2px;
}

.log-line.log-warn {
  border-left: 2px solid var(--warning);
  padding-left: 6px;
  margin-left: 2px;
}

.log-timestamp {
  color: #6b7280;
  min-width: 100px;
  flex-shrink: 0;
  font-size: 12px;
  font-family: var(--font-mono);
}

.log-level {
  flex-shrink: 0;
  min-width: 52px;
  text-align: center;
  font-size: 11px;
  font-weight: 600;
}

.log-level :deep(.el-tag__content) {
  font-size: 11px;
}

.log-message {
  color: #e5e7eb;
  flex: 1;
  word-break: break-word;
  overflow-wrap: anywhere;
  font-size: 13px;
  font-family: var(--font-mono);
}

.log-message :deep(mark) {
  background-color: rgba(99, 102, 241, 0.4);
  color: #e5e7eb;
  padding: 0 2px;
  border-radius: 2px;
}

.log-link {
  color: #6366f1;
  text-decoration: underline;
}

.log-link:hover {
  color: #4f46e5;
}

.log-meta {
  display: flex;
  gap: 8px;
  flex-shrink: 0;
  margin-left: auto;
  padding-left: 12px;
  font-family: var(--font-mono);
}

.meta-duration {
  color: #6b7280;
  font-size: 11px;
  background-color: rgba(255, 255, 255, 0.06);
  padding: 1px 6px;
  border-radius: 4px;
}

.meta-request {
  color: #6b7280;
  font-size: 11px;
  font-family: var(--font-mono);
  opacity: 0.7;
}

/* Floating Button */
.new-logs-btn {
  position: fixed;
  bottom: 24px;
  right: 24px;
  z-index: 500;
  box-shadow: 0 4px 12px rgba(0, 0, 0, 0.4);
}

/* Slide up transition */
.slide-up-enter-active,
.slide-up-leave-active {
  transition: transform 0.2s ease, opacity 0.2s ease;
}

.slide-up-enter-from,
.slide-up-leave-to {
  transform: translateY(20px);
  opacity: 0;
}

/* Responsive */
@media (max-width: 768px) {
  .page-header {
    flex-direction: column;
    align-items: flex-start;
  }

  .header-actions {
    width: 100%;
    justify-content: flex-start;
  }

  .toolbar-row {
    flex-direction: column;
    align-items: flex-start;
  }

  .toolbar-bottom {
    flex-direction: column;
    align-items: flex-start;
  }

  .search-group {
    width: 100%;
    max-width: none;
  }

  .log-terminal {
    padding: 10px;
    font-size: 12px;
  }

  .log-line {
    flex-wrap: wrap;
    gap: 4px 8px;
  }

  .log-timestamp {
    min-width: 70px;
  }

  .log-meta {
    display: none;
  }

  .new-logs-btn {
    bottom: 16px;
    right: 16px;
  }
}

@media (max-width: 480px) {
  .log-timestamp {
    min-width: 60px;
    font-size: 11px;
  }

  .log-level {
    min-width: 44px;
  }

  .log-level :deep(.el-tag__content) {
    font-size: 10px;
  }
}
</style>
