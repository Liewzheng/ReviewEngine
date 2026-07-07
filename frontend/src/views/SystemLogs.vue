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
          :type="isPaused ? 'warning' : 'default'"
          :icon="isPaused ? VideoPlay : VideoPause"
          @click="togglePause"
        >
          {{ isPaused ? 'Resume' : 'Pause' }}
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
    <div class="toolbar" :class="{ paused: isPaused }">
      <div class="toolbar-row">
        <!-- Level Filter -->
        <div class="filter-group">
          <span class="filter-label">Levels:</span>
          <el-checkbox-group v-model="selectedLevels" size="small">
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
            v-model="keyword"
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
          <span v-if="isPaused" class="pause-indicator">
            <el-icon><VideoPause /></el-icon>
            Paused — {{ bufferedLogs.length }} buffered
          </span>
          <span class="filter-count">
            Showing {{ filteredLogs.length }} of {{ logs.length }} logs
          </span>
        </div>
      </div>
    </div>

    <!-- Loading State -->
    <div v-if="loading" class="loading-container">
      <el-skeleton :rows="15" animated />
    </div>

    <!-- Log Terminal -->
    <div v-else ref="terminalRef" class="log-terminal" @scroll="handleScroll">
      <!-- Empty: No logs yet -->
      <div v-if="logs.length === 0" class="empty-state">
        <el-icon class="empty-icon" size="48"><Loading /></el-icon>
        <p>Waiting for logs...</p>
      </div>

      <!-- Empty: All filtered out -->
      <div v-else-if="filteredLogs.length === 0 && logs.length > 0" class="empty-state">
        <el-icon class="empty-icon" size="48"><InfoFilled /></el-icon>
        <p>No logs match current filters</p>
      </div>

      <!-- Empty: Cleared -->
      <div v-else-if="isCleared && logs.length === 0" class="empty-state">
        <el-icon class="empty-icon" size="48"><Check /></el-icon>
        <p>Logs cleared. New entries will appear here.</p>
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
        v-if="newLogCount > 0 && !autoScroll"
        type="primary"
        class="new-logs-btn"
        :icon="ArrowDown"
        @click="scrollToBottom"
      >
        {{ newLogCount }} new logs
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
import type { LogEntry, LogLevel, TimestampFormat } from '../types/logs'

// ==================== State ====================
const loading = ref(true)
const logs = ref<LogEntry[]>([])
const selectedLevels = ref<LogLevel[]>(['INFO', 'WARN', 'ERROR', 'DEBUG'])
const keyword = ref('')
const autoScroll = ref(true)
const timestampFormat = ref<TimestampFormat>('relative')
const isPaused = ref(false)
const bufferedLogs = ref<LogEntry[]>([])
const isCleared = ref(false)
const newLogCount = ref(0)
const downloading = ref(false)
const terminalRef = ref<HTMLElement | null>(null)

let autoScrollInterval: number | null = null
let newLogDismissTimer: number | null = null
let mockInterval: number | null = null

// ==================== Computed ====================
const filteredLogs = computed(() => {
  const kw = keyword.value.trim().toLowerCase()
  return logs.value.filter((log) => {
    const levelMatch = selectedLevels.value.includes(log.level)
    const keywordMatch = !kw || log.message.toLowerCase().includes(kw)
    return levelMatch && keywordMatch
  })
})

// ==================== Mock Data Generator ====================
const mockMessages: Record<LogLevel, string[]> = {
  INFO: [
    'Review completed for MR !{id}',
    'LLM provider initialized: {provider}',
    'Queue consumer started, {n} workers active',
    'Configuration reloaded from {path}',
    'Expert {name} registered successfully',
    'Webhook delivered to {url}',
    'Review batch processed: {n} items',
    'Token usage: {n} tokens this minute',
  ],
  WARN: [
    'LLM API rate limit approaching ({pct}%)',
    'Queue backlog growing: {n} pending reviews',
    'Retry attempt {n} for review {id}',
    'Slow response from {provider}: {ms}ms',
    'Memory usage at {pct}%',
    'Connection pool nearing limit: {n}/{max}',
  ],
  ERROR: [
    'LLM API timeout after {ms}ms',
    'Failed to parse review result for MR !{id}',
    'Database connection failed: {err}',
    'Authentication error for expert {id}',
    'Webhook delivery failed: {url} returned {code}',
    'Queue worker crashed, restarting...',
  ],
  DEBUG: [
    'Request payload: {json}',
    'Cache hit for key {key}',
    'Processing review {id} with prompt v{ver}',
    'Response headers: {headers}',
    'Parsed diff: {n} hunks, {m} lines',
  ],
}

const mockProviders = ['OpenAI', 'Anthropic', 'Gemini', 'Local']
const mockPaths = ['/etc/review-engine/config.yaml', '~/.review-engine/config.toml']
const mockNames = ['Security Expert', 'Performance Expert', 'Style Expert', 'Architecture Expert']
const mockUrls = ['https://gitlab.example.com/hooks/review', 'https://github.example.com/webhooks']

function rand<T>(arr: T[]): T { return arr[Math.floor(Math.random() * arr.length)] }
function randInt(min: number, max: number): number { return Math.floor(Math.random() * (max - min + 1)) + min }
function uid(): string { return Math.random().toString(36).slice(2, 10) }

function generateMockLog(index: number): LogEntry {
  const levels: LogLevel[] = ['INFO', 'WARN', 'ERROR', 'DEBUG']
  const weights = [0.5, 0.2, 0.1, 0.2]
  const r = Math.random()
  let cum = 0
  let level: LogLevel = 'INFO'
  for (let i = 0; i < levels.length; i++) {
    cum += weights[i]
    if (r < cum) { level = levels[i]; break }
  }

  const templates = mockMessages[level]
  let msg = rand(templates)
  msg = msg.replace('{id}', String(randInt(100, 9999)))
  msg = msg.replace('{provider}', rand(mockProviders))
  msg = msg.replace('{path}', rand(mockPaths))
  msg = msg.replace('{name}', rand(mockNames))
  msg = msg.replace('{url}', rand(mockUrls))
  msg = msg.replace('{n}', String(randInt(1, 500)))
  msg = msg.replace('{m}', String(randInt(10, 2000)))
  msg = msg.replace('{ms}', String(randInt(50, 5000)))
  msg = msg.replace('{pct}', String(randInt(60, 99)))
  msg = msg.replace('{max}', String(randInt(50, 100)))
  msg = msg.replace('{code}', String(randInt(400, 599)))
  msg = msg.replace('{err}', rand(['ECONNREFUSED', 'ETIMEDOUT', 'ENOTFOUND', 'EPIPE']))
  msg = msg.replace('{json}', '{"model":"gpt-4","temperature":0.2}')
  msg = msg.replace('{key}', `review:${uid()}`)
  msg = msg.replace('{ver}', String(randInt(1, 5)))
  msg = msg.replace('{headers}', 'content-type: application/json')
  msg = msg.replace('{headers}', 'x-request-id: ' + uid())

  const metadata: LogEntry['metadata'] = {}
  if (Math.random() > 0.5) metadata.durationMs = randInt(10, 2000)
  if (Math.random() > 0.7) metadata.requestId = uid()
  if (Math.random() > 0.8) metadata.reviewId = String(randInt(100, 9999))
  if (Math.random() > 0.9) metadata.expertId = String(randInt(1, 10))

  const now = new Date(Date.now() - index * randInt(100, 5000))

  return {
    id: uid() + '-' + index,
    timestamp: now.toISOString(),
    level,
    message: msg,
    metadata: Object.keys(metadata).length > 0 ? metadata : undefined,
  }
}

function generateMockLogs(count: number): LogEntry[] {
  const arr: LogEntry[] = []
  for (let i = count - 1; i >= 0; i--) {
    arr.push(generateMockLog(i))
  }
  return arr
}

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
  const kw = keyword.value.trim()
  if (!kw) return escapeHtml(msg)
  const re = new RegExp(`(${escapeRegExp(kw)})`, 'gi')
  return escapeHtml(msg).replace(re, '<mark>$1</mark>')
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
  if (isPaused.value) {
    // Resume: flush buffered logs
    logs.value.push(...bufferedLogs.value)
    trimLogs()
    bufferedLogs.value = []
    isPaused.value = false
    if (autoScroll.value) scrollToBottom()
  } else {
    isPaused.value = true
  }
}

function trimLogs() {
  if (logs.value.length > 5000) {
    logs.value = logs.value.slice(-5000)
  }
}

function confirmClear() {
  ElMessageBox.confirm(
    'Clear visible logs? This only affects the display, not stored logs.',
    'Clear Logs',
    { confirmButtonText: 'Clear', cancelButtonText: 'Cancel', type: 'warning' }
  ).then(() => {
    logs.value = []
    isCleared.value = true
    newLogCount.value = 0
  }).catch(() => {})
}

function downloadLogs() {
  downloading.value = true
  setTimeout(() => {
    const content = logs.value
      .map(l => `[${l.timestamp}] [${l.level}] ${l.message}`)
      .join('\n')
    const blob = new Blob([content], { type: 'text/plain' })
    const url = URL.createObjectURL(blob)
    const a = document.createElement('a')
    a.href = url
    a.download = `system-logs-${new Date().toISOString().slice(0, 19).replace(/:/g, '-')}.log`
    a.click()
    URL.revokeObjectURL(url)
    downloading.value = false
    ElNotification({
      title: 'Download Started',
      message: 'Your log file is being downloaded.',
      type: 'success',
      duration: 3000,
    })
  }, 800)
}

function scrollToBottom() {
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

// ==================== SSE / Mock Stream ====================
function addLogEntry(entry: LogEntry) {
  if (isPaused.value) {
    if (bufferedLogs.value.length < 1000) {
      bufferedLogs.value.push(entry)
    }
  } else {
    logs.value.push(entry)
    trimLogs()
    if (autoScroll.value) {
      scrollToBottom()
    } else {
      newLogCount.value++
      if (newLogDismissTimer) window.clearTimeout(newLogDismissTimer)
      newLogDismissTimer = window.setTimeout(() => { newLogCount.value = 0 }, 10000)
    }
  }
  isCleared.value = false
}

function startMockStream() {
  mockInterval = window.setInterval(() => {
    if (Math.random() > 0.7) return
    const entry = generateMockLog(0)
    entry.timestamp = new Date().toISOString()
    entry.id = uid() + '-' + Date.now()
    addLogEntry(entry)
  }, 2000)
}

// ==================== Lifecycle ====================
onMounted(() => {
  // Simulate initial fetch
  setTimeout(() => {
    logs.value = generateMockLogs(80)
    loading.value = false
    nextTick(() => {
      if (autoScroll.value) scrollToBottom()
    })
    startMockStream()
  }, 600)
})

onUnmounted(() => {
  if (mockInterval) clearInterval(mockInterval)
  if (autoScrollInterval) clearInterval(autoScrollInterval)
  if (newLogDismissTimer) clearTimeout(newLogDismissTimer)
})

// Watch auto-scroll changes
watch(autoScroll, (val) => {
  if (val) scrollToBottom()
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
  background-color: rgba(245, 158, 11, 0.08);
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
}

/* Empty States */
.empty-state {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  height: 100%;
  color: #6b7280;
  gap: 12px;
  padding: 48px 0;
}

.empty-icon {
  color: #6b7280;
  opacity: 0.6;
}

.empty-state p {
  margin: 0;
  font-size: 14px;
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
}

.log-message :deep(mark) {
  background-color: rgba(99, 102, 241, 0.4);
  color: #e5e7eb;
  padding: 0 2px;
  border-radius: 2px;
}

.log-meta {
  display: flex;
  gap: 8px;
  flex-shrink: 0;
  margin-left: auto;
  padding-left: 12px;
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
