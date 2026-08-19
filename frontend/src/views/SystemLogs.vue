<template>
  <div class="logs-page">
    <!-- Page Header -->
    <div class="page-header">
      <div class="header-title">
        <h2 class="page-title">{{ $t('logs.title') }}</h2>
        <p class="page-subtitle">{{ $t('logs.subtitle') }}</p>
      </div>
      <div class="header-actions">
        <el-button
          :type="logs.isPaused ? 'warning' : 'default'"
          :icon="logs.isPaused ? VideoPlay : VideoPause"
          @click="togglePause"
        >
          {{ logs.isPaused ? $t('logs.resume') : $t('logs.pause') }}
        </el-button>
        <el-button
          type="primary"
          :icon="Download"
          :loading="downloading"
          @click="downloadLogs"
        >
          {{ $t('logs.download') }}
        </el-button>
        <el-button
          type="danger"
          :icon="Delete"
          @click="confirmClear"
        >
          {{ $t('logs.clear') }}
        </el-button>
      </div>
    </div>

    <!-- Toolbar -->
    <div class="toolbar" :class="{ paused: logs.isPaused }">
      <div class="toolbar-row">
        <!-- Level Filter -->
        <div class="filter-group">
          <span class="filter-label">{{ $t('logs.levelsLabel') }}</span>
          <el-checkbox-group v-model="logs.levels" size="small">
            <el-checkbox value="INFO">
              <span class="level-dot" style="background-color: var(--info)"></span>
              INFO
            </el-checkbox>
            <el-checkbox value="WARN">
              <span class="level-dot" style="background-color: var(--warning)"></span>
              WARN
            </el-checkbox>
            <el-checkbox value="ERROR">
              <span class="level-dot" style="background-color: var(--error)"></span>
              ERROR
            </el-checkbox>
            <el-checkbox value="DEBUG">
              <span class="level-dot" style="background-color: var(--offline)"></span>
              DEBUG
            </el-checkbox>
          </el-checkbox-group>
        </div>

        <!-- Keyword Search -->
        <div class="search-group">
          <el-input
            v-model="searchInput"
            :placeholder="$t('logs.filterPlaceholder')"
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
            :active-text="$t('logs.autoScroll')"
            class="auto-scroll-switch"
          />

          <!-- Timestamp Format -->
          <div class="format-select">
            <span class="filter-label">{{ $t('logs.formatLabel') }}</span>
            <el-select v-model="timestampFormat" size="small" style="width: 120px">
              <el-option :label="$t('logs.format.relative')" value="relative" />
              <el-option :label="$t('logs.format.absolute')" value="absolute" />
              <el-option :label="$t('logs.format.iso')" value="iso" />
            </el-select>
          </div>
        </div>

        <div class="toolbar-right">
          <span v-if="logs.isPaused" class="pause-indicator">
            <el-icon><VideoPause /></el-icon>
            {{ $t('common.paused') }}
          </span>
          <span class="filter-count">
            {{ $t('logs.showingCount', { filtered: filteredLogs.length, total: logItems.length }) }}
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
        <el-empty :description="$t('logs.emptyCleared')">
          <template #image>
            <el-icon size="48" color="var(--offline)"><Check /></el-icon>
          </template>
        </el-empty>
      </div>

      <!-- Empty: No logs yet -->
      <div v-else-if="logItems.length === 0" class="empty-state">
        <el-empty :description="$t('logs.emptyWaiting')">
          <template #image>
            <el-icon size="48" color="var(--offline)" class="is-loading"><Loading /></el-icon>
          </template>
        </el-empty>
      </div>

      <!-- Empty: All filtered out -->
      <div v-else-if="filteredLogs.length === 0 && logItems.length > 0" class="empty-state">
        <el-empty :description="$t('logs.emptyFiltered')">
          <template #image>
            <el-icon size="48" color="var(--offline)"><InfoFilled /></el-icon>
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
        {{ logs.isPaused ? $t('logs.resume') : $t('logs.newLogs', { count: newLogCount }) }}
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
import { useI18n } from 'vue-i18n'
import type { LogLevel, TimestampFormat } from '../types/logs'
import { useLogs } from '../composables/useLogs'

// ==================== Composable ====================
const { t } = useI18n()
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
    logs.keyword = val
  }, 150)
})

// ==================== Computed ====================
const filteredLogs = computed(() => logs.filteredLogs)
const logItems = computed(() => logs.logs)

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
  if (sec < 60) return t('logs.time.secondsAgo', { n: sec })
  if (sec < 3600) return t('logs.time.minutesAgo', { n: Math.floor(sec / 60) })
  return t('logs.time.hoursAgo', { n: Math.floor(sec / 3600) })
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

// Highlight the log message safely. The raw message is first escaped via escapeHtml,
// so any regex replacements below operate on sanitized text and v-html receives safe HTML.
function highlightMessage(msg: string): string {
  let html = escapeHtml(msg)

  const kw = logs.keyword.trim()
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

function confirmClear() {
  ElMessageBox.confirm(
    t('logs.clearConfirm'),
    t('logs.clearTitle'),
    { confirmButtonText: t('logs.clear'), cancelButtonText: t('common.cancel'), type: 'warning' }
  ).then(() => {
    logs.clearLogs()
    isCleared.value = true
    newLogCount.value = 0
  }).catch(() => {})
}

async function downloadLogs() {
  try {
    await ElMessageBox.confirm(
      t('logs.downloadWarning'),
      t('logs.sensitiveTitle'),
      { confirmButtonText: t('logs.download'), cancelButtonText: t('common.cancel'), type: 'warning' }
    )
  } catch {
    return
  }

  downloading.value = true
  try {
    await logs.download()
    ElNotification({
      title: t('logs.downloadStartedTitle'),
      message: t('logs.downloadStartedMessage'),
      type: 'success',
      duration: 3000,
    })
  } catch {
    // error handled by composable
  } finally {
    downloading.value = false
  }
}

// Explicit user actions (the Resume/new-logs button, toggling auto-scroll)
// may resume a paused stream. The auto-scroll watcher only calls this when
// `!logs.isPaused`, so it never silently resumes the stream.
function scrollToBottom() {
  if (logs.isPaused) {
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
watch(() => logs.error, (err) => {
  if (err) {
    ElNotification({
      type: 'error',
      title: t('logs.streamErrorTitle'),
      message: err,
      duration: 5000,
    })
  }
})

// Watch for new logs: update the floating-button count, and pin the terminal
// to the latest entry when auto-scroll is on AND the user is not paused.
// Paused means the user is reading history — do not scroll, and never let the
// auto path resume the stream (scrollToBottom's unpause is for explicit clicks).
watch(() => logs.logs.length, (newLength, oldLength) => {
  if (oldLength !== undefined && newLength > oldLength) {
    isCleared.value = false
    if (!autoScroll.value && !logs.isPaused) {
      newLogCount.value++
      if (newLogDismissTimer) window.clearTimeout(newLogDismissTimer)
      newLogDismissTimer = window.setTimeout(() => { newLogCount.value = 0 }, 10000)
    }
  }
  if (autoScroll.value && !logs.isPaused) {
    scrollToBottom()
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
  background-color: var(--bg-surface);
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
  --el-empty-description-color: var(--text-secondary);
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
  background-color: var(--bg-hover);
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
  color: var(--text-secondary);
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
  color: var(--text-primary);
  flex: 1;
  word-break: break-word;
  overflow-wrap: anywhere;
  font-size: 13px;
  font-family: var(--font-mono);
}

.log-message :deep(mark) {
  background-color: rgba(99, 102, 241, 0.4);
  color: var(--text-primary);
  padding: 0 2px;
  border-radius: 2px;
}

.log-link {
  color: var(--brand);
  text-decoration: underline;
}

.log-link:hover {
  color: var(--brand-hover);
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
  color: var(--text-secondary);
  font-size: 11px;
  background-color: var(--bg-hover);
  padding: 1px 6px;
  border-radius: 4px;
}

.meta-request {
  color: var(--text-secondary);
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
