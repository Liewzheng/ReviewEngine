<script setup lang="ts">
import { computed } from 'vue'
import { Link, DocumentCopy, Download, ArrowRight } from '@element-plus/icons-vue'
import { ElNotification } from 'element-plus'
import { useUpgrade } from '../../composables/useUpgrade'
import type { InstallMethod } from '../../types/upgrade'

// The composable is a module-scope singleton, so App.vue and this dialog share
// the same check/status/polling state. Destructured refs stay top-level
// bindings and are auto-unwrapped in the template.
const { check, checking, status, starting, dockerInfo, error, start, stopPolling } = useUpgrade()

const visible = defineModel<boolean>({ required: true })

const INSTALL_SOURCE_LABELS: Record<InstallMethod, string> = {
  binary: 'prebuilt binary',
  brew: 'Homebrew',
  docker: 'Docker container',
  cargo: 'cargo install',
  unknown: 'unknown install method',
}

const installSourceLabel = computed(() =>
  check.value ? (INSTALL_SOURCE_LABELS[check.value.installMethod] ?? 'unknown install method') : ''
)

const isBinary = computed(() => check.value?.installMethod === 'binary')
const isDocker = computed(() => check.value?.installMethod === 'docker')

// Command shown in the docker form comes from the POST response; everywhere
// else it is the check response's `upgradeHint`.
const commandToCopy = computed(() => {
  if (!check.value) return ''
  if (isDocker.value) return dockerInfo.value?.instructions || check.value.upgradeHint
  return check.value.upgradeHint
})

const STEP_ORDER = ['checking', 'downloading', 'verifying', 'installing']
const stepIndex = computed(() => {
  const st = status.value?.state
  if (!st) return -1
  const idx = STEP_ORDER.indexOf(st)
  return idx >= 0 ? idx : -1
})
const inProgress = computed(() => {
  const st = status.value?.state
  return !!st && STEP_ORDER.includes(st)
})

async function copy(text: string) {
  try {
    await navigator.clipboard.writeText(text)
    ElNotification.success({ title: 'Copied', message: 'Command copied to clipboard.', duration: 2000 })
  } catch {
    ElNotification.warning({ title: 'Copy failed', message: 'Could not copy to clipboard.', duration: 2000 })
  }
}
</script>

<template>
  <el-dialog
    v-model="visible"
    title="Upgrade Review Engine"
    width="560px"
    align-center
    @closed="stopPolling"
  >
    <!-- Loading: check not resolved yet -->
    <div v-if="!check" class="upgrade-content">
      <el-skeleton v-if="checking" :rows="3" animated />
      <el-empty v-else :description="error || 'Unable to check for updates'" />
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
          Release notes <el-icon><Link /></el-icon>
        </a>
      </div>

      <!-- kimi-style hint -->
      <div class="kimi-hint">
        <p>A newer version <b>v{{ check.latestVersion }}</b> is available (current: v{{ check.currentVersion }}).</p>
        <p>Detected install source: <b>{{ installSourceLabel }}.</b></p>
        <template v-if="!isDocker">
          <p>To update, run:</p>
          <div class="command-block">
            <code>{{ commandToCopy }}</code>
            <el-button size="small" text :icon="DocumentCopy" @click="copy(commandToCopy)">Copy</el-button>
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

      <!-- binary form: automated upgrade -->
      <div v-if="isBinary" class="form-section">
        <template v-if="inProgress">
          <el-steps :active="stepIndex" finish-status="success" process-status="process" align-center class="upgrade-steps">
            <el-step title="Checking" />
            <el-step title="Downloading" />
            <el-step title="Verifying" />
            <el-step title="Installing" />
          </el-steps>
          <p class="step-message">{{ status?.message }}</p>
        </template>

        <el-alert
          v-else-if="status?.state === 'done'"
          :title="status?.message || '升级完成，服务需重启后生效'"
          type="success"
          :closable="false"
          show-icon
          class="upgrade-alert"
        />

        <el-alert
          v-else-if="status?.state === 'failed'"
          :title="status?.message || '升级失败'"
          type="error"
          :closable="false"
          show-icon
          class="upgrade-alert"
        >
          <template #default>
            <el-button size="small" type="danger" plain :icon="Download" :loading="starting" @click="start">
              Retry
            </el-button>
          </template>
        </el-alert>

        <template v-else-if="check.platformAssetAvailable">
          <div class="confirm-row">
            <el-button type="primary" :icon="Download" :loading="starting" @click="start">
              Upgrade Now
            </el-button>
            <span class="confirm-hint">The server binary will be replaced; a restart is required for it to take effect.</span>
          </div>
        </template>

        <el-alert
          v-else
          title="No release asset is available for this platform, so automatic upgrade is not possible."
          type="warning"
          :closable="false"
          show-icon
          class="upgrade-alert"
        />
      </div>

      <!-- docker form: instructions on the host machine + note -->
      <div v-else-if="isDocker" class="form-section">
        <p class="section-label">On the host machine, run:</p>
        <div class="command-block">
          <code>{{ commandToCopy }}</code>
          <el-button size="small" text :icon="DocumentCopy" @click="copy(commandToCopy)">Copy</el-button>
        </div>
        <el-alert
          v-if="dockerInfo?.note"
          :title="dockerInfo.note"
          type="warning"
          :closable="false"
          show-icon
          class="upgrade-alert"
        />
        <el-skeleton v-else :rows="2" animated />
      </div>

      <!-- brew / cargo / unknown: hint command already shown above with copy -->
    </div>

    <template #footer>
      <el-button @click="visible = false">Close</el-button>
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
