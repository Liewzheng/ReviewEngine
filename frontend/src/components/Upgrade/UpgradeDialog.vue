<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { Link, DocumentCopy, Download, ArrowRight } from '@element-plus/icons-vue'
import { ElNotification } from 'element-plus'
import { useUpgrade } from '../../composables/useUpgrade'
import type { InstallMethod } from '../../types/upgrade'

// The composable is a module-scope singleton, so App.vue and this dialog share
// the same check/status/polling state. Destructured refs stay top-level
// bindings and are auto-unwrapped in the template.
const { check, checking, status, starting, error, start, stopPolling } = useUpgrade()

const { t } = useI18n()

const visible = defineModel<boolean>({ required: true })

const INSTALL_SOURCE_KEYS: Record<InstallMethod, string> = {
  binary: 'upgrade.source.binary',
  brew: 'upgrade.source.brew',
  docker: 'upgrade.source.docker',
  cargo: 'upgrade.source.cargo',
  unknown: 'upgrade.source.unknown',
}

const installSourceLabel = computed(() =>
  check.value ? t(INSTALL_SOURCE_KEYS[check.value.installMethod] ?? 'upgrade.source.unknown') : ''
)

const isBinary = computed(() => check.value?.installMethod === 'binary')
const isDocker = computed(() => check.value?.installMethod === 'docker')
// Binary and docker both run the automated in-process upgrade; brew/cargo/
// unknown only get a copyable hint command.
const isAutomated = computed(() => isBinary.value || isDocker.value)

// Hint command shown for non-automated methods (and binary as a manual
// fallback). Docker never shows a host command.
const commandToCopy = computed(() => check.value?.upgradeHint ?? '')

// Progress steps: docker adds a final "Restarting" step after installing.
const DOCKER_STEP_KEYS = ['checking', 'downloading', 'verifying', 'installing', 'restarting']
const BINARY_STEP_KEYS = ['checking', 'downloading', 'verifying', 'installing']
const stepKeys = computed(() => (isDocker.value ? DOCKER_STEP_KEYS : BINARY_STEP_KEYS))
const stepTitles = computed(() => stepKeys.value.map((k) => t(`upgrade.step.${k}`)))
const stepIndex = computed(() => {
  const st = status.value?.state
  if (!st) return -1
  const idx = stepKeys.value.indexOf(st)
  return idx >= 0 ? idx : -1
})
const inProgress = computed(() => {
  const st = status.value?.state
  return !!st && (st === 'checking' || st === 'downloading' || st === 'verifying' || st === 'installing')
})
const isRestarting = computed(() => status.value?.state === 'restarting')

// ---- Download progress (state === 'downloading') ----
// The backend reports byte counters on each 2s poll. Speed is derived from
// consecutive samples and EMA-smoothed to avoid jumpiness; ETA divides the
// remaining bytes by that smoothed speed. Both show '—' until the first
// usable sample pair exists.

/** Byte counters while downloading; null degrades the UI to the plain step message. */
const downloadInfo = computed(() => {
  const s = status.value
  if (s?.state === 'downloading' && s.download && s.download.totalBytes > 0) {
    return s.download
  }
  return null
})

const downloadPercent = computed(() => {
  const d = downloadInfo.value
  if (!d) return 0
  return Math.min(100, Math.floor((d.downloadedBytes / d.totalBytes) * 100))
})

const prevSample = ref<{ bytes: number; time: number } | null>(null)
const smoothSpeed = ref(0) // bytes/s, EMA of per-poll instantaneous speed

function resetDownloadSample() {
  prevSample.value = null
  smoothSpeed.value = 0
}

watch(status, (s) => {
  const d = s?.state === 'downloading' ? s.download : null
  if (!d || d.totalBytes <= 0) {
    resetDownloadSample()
    return
  }
  const now = Date.now()
  const prev = prevSample.value
  if (prev) {
    const dt = (now - prev.time) / 1000
    const inst = dt > 0 ? (d.downloadedBytes - prev.bytes) / dt : 0
    if (inst > 0) {
      smoothSpeed.value = smoothSpeed.value > 0 ? 0.5 * smoothSpeed.value + 0.5 * inst : inst
    }
  }
  prevSample.value = { bytes: d.downloadedBytes, time: now }
})

// Drop stale samples when the dialog closes mid-download.
watch(visible, (v) => {
  if (!v) resetDownloadSample()
})

/** B/KB/MB/GB with 1 decimal (bytes shown whole). */
function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return '0 B'
  if (bytes < 1024) return `${Math.floor(bytes)} B`
  const units = ['KB', 'MB', 'GB']
  let value = bytes / 1024
  let unit = 0
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024
    unit += 1
  }
  return `${value.toFixed(1)} ${units[unit]}`
}

/** Compact localized ETA: "<60s" → seconds-only, otherwise minutes+seconds. */
function formatEta(totalSeconds: number): string {
  const s = Math.max(0, Math.round(totalSeconds))
  if (s < 60) return t('upgrade.etaSeconds', { s })
  return t('upgrade.etaMinutesSeconds', { m: Math.floor(s / 60), s: s % 60 })
}

const speedText = computed(() =>
  smoothSpeed.value > 0 ? `${formatBytes(smoothSpeed.value)}/s` : '—'
)
const etaText = computed(() => {
  const d = downloadInfo.value
  if (!d || smoothSpeed.value <= 0) return '—'
  return formatEta((d.totalBytes - d.downloadedBytes) / smoothSpeed.value)
})
const downloadTotalTitle = computed(() => {
  const d = downloadInfo.value
  return d ? `${t('upgrade.downloadTotal')} ${formatBytes(d.totalBytes)}` : ''
})

// Done / confirm copy differs between binary and docker (docker restarts itself).
const doneTitle = computed(() => {
  const msg = status.value?.message
  if (msg) return msg
  return isDocker.value ? t('upgrade.dockerDone') : t('upgrade.doneFallback')
})
const upgradeHint = computed(() => (isDocker.value ? t('upgrade.dockerHint') : t('upgrade.binaryHint')))

async function copy(text: string) {
  try {
    await navigator.clipboard.writeText(text)
    ElNotification.success({ title: t('common.copied'), message: t('common.commandCopied'), duration: 2000 })
  } catch {
    ElNotification.warning({ title: t('common.copyFailed'), message: t('common.copyFailedMessage'), duration: 2000 })
  }
}
</script>

<template>
  <el-dialog
    v-model="visible"
    :title="$t('upgrade.title')"
    width="560px"
    align-center
    @closed="stopPolling"
  >
    <!-- Loading: check not resolved yet -->
    <div v-if="!check" class="upgrade-content">
      <el-skeleton v-if="checking" :rows="3" animated />
      <el-empty v-else :description="error || $t('upgrade.checkFailed')" />
    </div>

    <div v-else class="upgrade-content">
      <!-- current → latest + release link -->
      <div class="version-row">
        <span class="version-chip-from">v{{ check.currentVersion }}</span>
        <el-icon class="version-arrow"><ArrowRight /></el-icon>
        <span class="version-chip-to">v{{ check.latestVersion }}</span>
        <a
          v-if="check.releaseUrl"
          :href="check.releaseUrl"
          target="_blank"
          rel="noopener"
          class="release-link"
        >
          {{ $t('upgrade.releaseNotes') }} <el-icon><Link /></el-icon>
        </a>
      </div>

      <!-- kimi-style hint -->
      <div class="kimi-hint">
        <p>{{ $t('upgrade.newerPrefix') }} <b>v{{ check.latestVersion }}</b> {{ $t('upgrade.newerSuffix', { current: check.currentVersion }) }}</p>
        <p>{{ $t('upgrade.detectedPrefix') }} <b>{{ installSourceLabel }}.</b></p>
        <template v-if="!isDocker">
          <p>{{ $t('upgrade.runInstruction') }}</p>
          <div class="command-block">
            <code>{{ commandToCopy }}</code>
            <el-button size="small" text :icon="DocumentCopy" @click="copy(commandToCopy)">{{ $t('common.copy') }}</el-button>
          </div>
        </template>
      </div>

      <!-- error banner (409 / failed POST / check errors) -->
      <el-alert
        v-if="error"
        :title="error"
        type="error"
        :closable="false"
        show-icon
        class="upgrade-alert"
      />

      <!-- automated upgrade form (binary + docker) -->
      <div v-if="isAutomated" class="form-section">
        <el-steps
          v-if="inProgress || isRestarting"
          :active="isRestarting ? stepKeys.length - 1 : stepIndex"
          finish-status="success"
          process-status="process"
          align-center
          class="upgrade-steps"
        >
          <el-step v-for="(title, i) in stepTitles" :key="i" :title="title" />
        </el-steps>
        <p v-if="inProgress" class="step-message">{{ status?.message }}</p>

        <!-- download progress: byte counters, speed and ETA (only while
             the backend reports download progress) -->
        <div v-if="downloadInfo" class="download-progress">
          <el-progress :percentage="downloadPercent" :stroke-width="10" class="download-bar" />
          <div class="download-stats">
            <span :title="downloadTotalTitle">
              {{ formatBytes(downloadInfo.downloadedBytes) }} / {{ formatBytes(downloadInfo.totalBytes) }}
            </span>
            <span>{{ $t('upgrade.downloadSpeed') }} {{ speedText }}</span>
            <span>{{ $t('upgrade.downloadEta') }} {{ etaText }}</span>
          </div>
        </div>

        <el-alert
          v-if="isRestarting"
          :title="status?.message || $t('upgrade.restartHint')"
          type="warning"
          :closable="false"
          show-icon
          class="upgrade-alert"
        />

        <el-alert
          v-else-if="status?.state === 'done'"
          :title="doneTitle"
          type="success"
          :closable="false"
          show-icon
          class="upgrade-alert"
        />

        <el-alert
          v-else-if="status?.state === 'failed'"
          :title="status?.message || $t('upgrade.failedFallback')"
          type="error"
          :closable="false"
          show-icon
          class="upgrade-alert"
        >
          <template #default>
            <el-button size="small" type="danger" plain :icon="Download" :loading="starting" @click="start">
              {{ $t('common.retry') }}
            </el-button>
          </template>
        </el-alert>

        <template v-else-if="isDocker || check.platformAssetAvailable">
          <div class="confirm-row">
            <el-button type="primary" :icon="Download" :loading="starting" @click="start">
              {{ $t('upgrade.upgradeNow') }}
            </el-button>
            <span class="confirm-hint">{{ upgradeHint }}</span>
          </div>
        </template>

        <el-alert
          v-else
          :title="$t('upgrade.noAsset')"
          type="warning"
          :closable="false"
          show-icon
          class="upgrade-alert"
        />
      </div>

      <!-- brew / cargo / unknown: hint command already shown above with copy -->
    </div>

    <template #footer>
      <el-button @click="visible = false">{{ $t('common.close') }}</el-button>
    </template>
  </el-dialog>
</template>

<style scoped>
.upgrade-content {
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.version-row {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}

.version-chip-from,
.version-chip-to {
  font-family: var(--font-mono);
  font-size: 13px;
  font-weight: 600;
  padding: 3px 10px;
  border-radius: var(--radius-sm);
}

.version-chip-from {
  color: var(--text-secondary);
  background: var(--bg-card);
  border: 1px solid var(--border-color);
}

.version-chip-to {
  color: var(--success);
  background: rgba(34, 197, 94, 0.12);
  border: 1px solid rgba(34, 197, 94, 0.35);
}

.version-arrow {
  color: var(--text-secondary);
}

.release-link {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  margin-left: auto;
  font-size: 13px;
  font-weight: 500;
  color: var(--brand);
  text-decoration: none;
}

.release-link:hover {
  color: var(--brand-hover);
}

.kimi-hint {
  background: var(--bg-card);
  border: 1px solid var(--border-color);
  border-radius: var(--radius-md);
  padding: 12px 16px;
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.kimi-hint p {
  margin: 0;
  font-size: 13px;
  color: var(--text-primary);
  line-height: 1.5;
}

.command-block {
  display: flex;
  align-items: center;
  gap: 8px;
  background: var(--bg-surface);
  border: 1px solid var(--border-color);
  border-radius: var(--radius-sm);
  padding: 6px 8px 6px 12px;
}

.command-block code {
  flex: 1;
  font-family: var(--font-mono);
  font-size: 12px;
  color: var(--text-primary);
  white-space: pre-wrap;
  word-break: break-all;
}

.form-section {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.section-label {
  margin: 0;
  font-size: 13px;
  font-weight: 600;
  color: var(--text-secondary);
}

.upgrade-steps {
  padding: 4px 0;
}

.step-message {
  margin: 0;
  text-align: center;
  font-size: 13px;
  color: var(--text-secondary);
}

.download-progress {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.download-bar :deep(.el-progress-bar__outer) {
  background-color: var(--bg-surface);
}

.download-bar :deep(.el-progress__text) {
  font-size: 12px;
  color: var(--text-secondary);
}

.download-stats {
  display: flex;
  justify-content: center;
  gap: 12px;
  flex-wrap: wrap;
  font-size: 12px;
  color: var(--text-secondary);
  font-variant-numeric: tabular-nums;
}

.confirm-row {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: 8px;
}

.confirm-hint {
  font-size: 12px;
  color: var(--text-secondary);
}

.upgrade-alert {
  width: 100%;
}
</style>
