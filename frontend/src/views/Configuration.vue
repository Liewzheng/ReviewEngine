<template>
  <div class="config-page">
    <!-- Page Header -->
    <div class="page-header">
      <div class="header-left">
        <h2 class="page-title">{{ $t('config.title') }}</h2>
        <p class="page-subtitle">
          {{ isEditing ? $t('config.subtitle.edit') : $t('config.subtitle.view') }}
        </p>
      </div>
      <div class="header-actions">
        <template v-if="!isEditing">
          <el-button type="primary" @click="enterEditMode">
            <el-icon><Edit /></el-icon>
            <span>{{ $t('config.editBtn') }}</span>
          </el-button>
          <el-button @click="refreshConfig">
            <el-icon><Refresh /></el-icon>
            <span>{{ $t('common.refresh') }}</span>
          </el-button>
        </template>
        <template v-else>
          <!-- Tooltip explains why Save is disabled; the wrapper span is needed
               because a disabled button swallows pointer events. -->
          <el-tooltip :content="$t('config.noChangesToSave')" :disabled="dirty" placement="top">
            <span class="save-button-wrapper">
              <el-badge :is-dot="dirty" type="danger">
                <el-button type="primary" :loading="saving" :disabled="!dirty" @click="saveChanges">
                  <el-icon><Check /></el-icon>
                  <span>{{ $t('common.saveChanges') }}</span>
                </el-button>
              </el-badge>
            </span>
          </el-tooltip>
          <el-button @click="cancelEdit">
            <el-icon><Close /></el-icon>
            <span>{{ $t('common.cancel') }}</span>
          </el-button>
        </template>
      </div>
    </div>

    <!-- Loading Skeleton -->
    <div v-if="loading" class="skeleton-container">
      <el-card v-for="n in 3" :key="n" class="skeleton-card">
        <el-skeleton :rows="5" animated />
      </el-card>
    </div>

    <!-- Empty State -->
    <el-empty v-else-if="loadError" :description="$t('config.loadFailed')" />

    <!-- Form -->
    <el-form
      v-else
      ref="formRef"
      :model="config"
      :rules="rules"
      :disabled="!isEditing"
      :label-position="labelPosition"
      label-width="auto"
      class="config-form"
      @submit.prevent
    >
      <!-- Git Platforms Card -->
      <GitPlatformsSection
        ref="gitPlatformsCardRef"
        :platforms="config.gitPlatforms"
        :is-editing="isEditing"
        @add="addGitPlatform"
        @edit="editGitPlatform"
        @remove="removeGitPlatform"
      />

      <!-- Review Rules Card -->
      <el-card ref="rulesCardRef" class="config-card">
        <template #header>
          <div class="card-header">
            <el-icon><Collection /></el-icon>
            <span>{{ $t('config.rules.title') }}</span>
          </div>
        </template>
        <div class="card-body">
          <el-row :gutter="20">
            <el-col :xs="24" :sm="12">
              <el-form-item :label="$t('config.rules.minScore')" prop="rules.minScore">
                <div class="slider-with-value">
                  <el-slider v-model="config.rules.minScore" :disabled="!isEditing" :min="0" :max="100" :step="5" />
                  <span class="slider-value">{{ config.rules.minScore }}</span>
                </div>
              </el-form-item>
            </el-col>
            <el-col :xs="24" :sm="12">
              <el-form-item :label="$t('config.rules.maxDuration')" prop="rules.maxReviewDurationSeconds">
                <!-- 0 means "unlimited" (the backend seeds the UI projection
                     with 0 when the field was never configured, and nothing
                     backend-side consumes a fabricated minimum). Render 0 as
                     an empty input with a placeholder instead of letting the
                     spinbutton clamp the display to 30; clearing the input
                     writes 0 back, so saving keeps 0 as 0. -->
                <el-input-number
                  v-model="maxDurationInput"
                  :disabled="!isEditing"
                  :min="0"
                  :max="3600"
                  :step="30"
                  :placeholder="$t('config.rules.maxDurationUnlimited')"
                  style="width: 100%"
                />
              </el-form-item>
            </el-col>
            <el-col :xs="24" :sm="12">
              <el-form-item :label="$t('config.rules.blockOnCritical')" prop="rules.blockOnCritical">
                <el-switch v-model="config.rules.blockOnCritical" :disabled="!isEditing" />
              </el-form-item>
            </el-col>
            <el-col :xs="24" :sm="12">
              <el-form-item :label="$t('config.rules.autoCommentOnPass')" prop="rules.autoCommentOnPass">
                <el-switch v-model="config.rules.autoCommentOnPass" :disabled="!isEditing" />
              </el-form-item>
            </el-col>
            <el-col :xs="24">
              <el-form-item :label="$t('config.rules.commentTemplate')" prop="rules.commentTemplate">
                <el-input
                  v-model="config.rules.commentTemplate"
                  :disabled="!isEditing"
                  type="textarea"
                  :rows="4"
                  :maxlength="2000"
                  show-word-limit
                  :placeholder="$t('config.rules.commentTemplatePlaceholder')"
                />
              </el-form-item>
            </el-col>
            <el-col :xs="24">
              <el-form-item :label="$t('config.rules.excludedPatterns')" prop="rules.excludedPatterns">
                <div class="tag-input">
                  <el-tag
                    v-for="(pattern, index) in config.rules.excludedPatterns"
                    :key="index"
                    closable
                    :disable-transitions="false"
                    @close="removePattern(index)"
                  >
                    {{ pattern }}
                  </el-tag>
                  <el-input
                    v-if="patternInputVisible"
                    :ref="setPatternInputRef"
                    v-model="patternInputValue"
                    size="small"
                    @keyup.enter="addPattern"
                    @blur="addPattern"
                  />
                  <el-button v-else size="small" @click="showPatternInput">
                    <el-icon><Plus /></el-icon>
                    {{ $t('config.rules.addPattern') }}
                  </el-button>
                </div>
              </el-form-item>
            </el-col>
            <el-col :xs="24">
              <el-form-item :label="$t('config.rules.requiredExperts')" prop="rules.requiredExperts">
                <el-checkbox-group v-model="config.rules.requiredExperts" :disabled="!isEditing">
                  <el-checkbox value="Security" label="Security" />
                  <el-checkbox value="Performance" label="Performance" />
                  <el-checkbox value="Quality" label="Quality" />
                  <el-checkbox value="Maintainability" label="Maintainability" />
                  <el-checkbox value="Test Coverage" label="Test Coverage" />
                  <el-checkbox value="Documentation" label="Documentation" />
                  <el-checkbox value="Dependencies" label="Dependencies" />
                </el-checkbox-group>
              </el-form-item>
            </el-col>
          </el-row>
        </div>
      </el-card>

      <!-- Advanced Toggle -->
      <div class="advanced-toggle">
        <el-button link type="primary" :disabled="false" @click="showAdvanced = !showAdvanced">
          <el-icon v-if="showAdvanced"><ArrowUp /></el-icon>
          <el-icon v-else><ArrowDown /></el-icon>
          {{ showAdvanced ? $t('config.advanced.hide') : $t('config.advanced.show') }}
        </el-button>
      </div>

      <!-- Advanced Card -->
      <el-card v-show="showAdvanced" ref="advancedCardRef" class="config-card">
        <template #header>
          <div class="card-header">
            <el-icon><Tools /></el-icon>
            <span>{{ $t('config.advanced.title') }}</span>
          </div>
        </template>
        <div class="card-body">
          <el-row :gutter="20">
            <el-col :xs="24" :sm="12">
              <el-form-item :label="$t('config.advanced.logLevel')" prop="advanced.logLevel">
                <el-select v-model="config.advanced.logLevel" :disabled="!isEditing" style="width: 100%">
                  <el-option label="Debug" value="debug" />
                  <el-option label="Info" value="info" />
                  <el-option label="Warn" value="warn" />
                  <el-option label="Error" value="error" />
                </el-select>
              </el-form-item>
            </el-col>
            <el-col :xs="24" :sm="12">
              <el-form-item :label="$t('config.advanced.logRetention')" prop="advanced.logRetentionDays">
                <el-input-number v-model="config.advanced.logRetentionDays" :disabled="!isEditing" :min="1" :max="90" style="width: 100%" />
              </el-form-item>
            </el-col>
            <el-col :xs="24" :sm="12">
              <el-form-item :label="$t('config.advanced.sseHeartbeat')" prop="advanced.sseHeartbeatInterval">
                <el-input-number v-model="config.advanced.sseHeartbeatInterval" :disabled="!isEditing" :min="5" :max="60" style="width: 100%" />
              </el-form-item>
            </el-col>
            <el-col :xs="24" :sm="12">
              <el-form-item :label="$t('config.advanced.maxConcurrent')" prop="advanced.maxConcurrentReviews">
                <el-input-number v-model="config.advanced.maxConcurrentReviews" :disabled="!isEditing" :min="1" :max="20" style="width: 100%" />
              </el-form-item>
            </el-col>
            <el-col :xs="24" :sm="12">
              <el-form-item :label="$t('config.advanced.requestTimeout')" prop="advanced.requestTimeout">
                <el-input-number v-model="config.advanced.requestTimeout" :disabled="!isEditing" :min="10" :max="300" style="width: 100%" />
              </el-form-item>
            </el-col>
            <el-col :xs="24" :sm="12">
              <el-form-item :label="$t('config.advanced.enableMetrics')" prop="advanced.enableMetrics">
                <el-switch v-model="config.advanced.enableMetrics" :disabled="!isEditing" />
              </el-form-item>
            </el-col>
            <el-col :xs="24" :sm="12">
              <el-form-item :label="$t('config.advanced.debugMode')" prop="advanced.debugMode">
                <el-switch v-model="config.advanced.debugMode" :disabled="!isEditing" />
              </el-form-item>
            </el-col>
            <!-- Runtime info, not config: no `prop`, stays disabled in edit
                 mode, and the whole row is hidden until /system/health
                 answers (fail-silent on error). -->
            <el-col v-if="storageBackend" :xs="24" :sm="12">
              <el-form-item :label="$t('config.advanced.storageBackend')">
                <el-input :model-value="storageBackendLabel" disabled readonly />
              </el-form-item>
            </el-col>
          </el-row>
        </div>
      </el-card>
    </el-form>

    <!-- Mobile Sticky Actions -->
    <div v-if="isEditing" class="mobile-actions">
      <el-tooltip :content="$t('config.noChangesToSave')" :disabled="dirty" placement="top">
        <span class="save-button-wrapper">
          <el-badge :is-dot="dirty" type="danger" class="mobile-badge">
            <el-button type="primary" :loading="saving" :disabled="!dirty" @click="saveChanges">
              {{ $t('common.saveChanges') }}
            </el-button>
          </el-badge>
        </span>
      </el-tooltip>
      <el-button @click="cancelEdit">{{ $t('common.cancel') }}</el-button>
    </div>
  </div>
</template>

<script setup lang="ts">
import { ref, computed, watch, onMounted, onUnmounted } from 'vue'
import { onBeforeRouteLeave } from 'vue-router'
import {
  ArrowDown,
  ArrowUp,
  Check,
  Close,
  Collection,
  Edit,
  Plus,
  Refresh,
  Tools,
} from '@element-plus/icons-vue'
import { ElMessageBox, ElNotification } from 'element-plus'
import { useI18n } from 'vue-i18n'
import { useConfig } from '../composables/useConfig'
import { useConfigForm } from '../composables/useConfigForm'
import { getSystemHealth } from '../services/health'
import type { AppConfig, GitPlatformConfig } from '../types/config'
import type { StorageBackendKind } from '../types/dashboard'
import GitPlatformsSection from '../components/Config/GitPlatformsSection.vue'

// --- Composables ---
const { t } = useI18n()
const cfg = useConfig()

const {
  config,
  isEditing,
  formRef,
  configDirty,
  rules,
  patternInputVisible,
  patternInputValue,
  setPatternInputRef,
  showPatternInput,
  addPattern,
  removePattern,
  enterEditMode,
  restoreSnapshot,
  commitSnapshot,
  loadConfig,
  refreshConfig,
} = useConfigForm(cfg)

// --- State ---
const loading = cfg.loading
const loadError = computed(() => !!cfg.error.value)
const saving = cfg.saving
const showAdvanced = ref(false)

/* Read-only runtime info: the persistence backend in use, from
 * GET /system/health (`storage_backend`, 0.10.0). Fail-silent — a health
 * check error or an older server simply leaves the row hidden. */
const storageBackend = ref<StorageBackendKind | null>(null)

const storageBackendLabel = computed(() =>
  storageBackend.value ? t(`config.advanced.storageBackendKind.${storageBackend.value}`) : ''
)

function loadStorageBackend() {
  getSystemHealth()
    .then((health) => {
      storageBackend.value = health.storageBackend ?? null
    })
    .catch(() => {})
}

// Card refs for flash animation
const gitPlatformsCardRef = ref<HTMLElement>()
const rulesCardRef = ref<HTMLElement>()
const advancedCardRef = ref<HTMLElement>()

// Responsive layout
const windowWidth = ref(window.innerWidth)
const labelPosition = computed(() => (windowWidth.value >= 1024 ? 'left' : 'top'))

// --- Computed ---
const dirty = computed(() => configDirty.value)

// Display proxy for `rules.maxReviewDurationSeconds`: the stored 0 renders as
// an empty input (placeholder explains "0 = unlimited") instead of a fake 30;
// clearing the input stores 0 again. The underlying config model only ever
// holds real numbers, so save/snapshot/dirty tracking are unaffected.
const maxDurationInput = computed<number | undefined>({
  get: () => (config.rules.maxReviewDurationSeconds === 0 ? undefined : config.rules.maxReviewDurationSeconds),
  set: (val) => {
    config.rules.maxReviewDurationSeconds = val ?? 0
  },
})

// --- Methods ---
// Git platform rows live on the main reactive `config`, so these mutations feed
// the existing configDirty JSON comparison and ride along in the PUT /config
// payload (full-replace semantics; blank/masked secrets keep stored values).
function addGitPlatform(entry: GitPlatformConfig) {
  config.gitPlatforms.push(entry)
  ElNotification({
    title: t('config.gitPlatforms.addedTitle'),
    message: t('config.gitPlatforms.addedMessage'),
    type: 'info',
    duration: 3000,
  })
}

function editGitPlatform(index: number, entry: GitPlatformConfig) {
  config.gitPlatforms.splice(index, 1, entry)
}

function removeGitPlatform(index: number) {
  config.gitPlatforms.splice(index, 1)
}

function cancelEdit() {
  restoreSnapshot()
}

async function saveChanges() {
  if (!formRef.value) return
  const valid = await formRef.value.validate().catch(() => false)
  // Missing/incorrect fields keep their inline validation errors, but they
  // must not block saving: the backend treats empty secret/token fields as
  // "keep the stored value", so a partially-filled form saves safely. Warn,
  // then save with whatever is present.
  if (!valid) {
    ElNotification({
      title: t('config.validation.title'),
      message: t('config.validation.saveWithWarnings'),
      type: 'warning',
      duration: 4000,
    })
  }

  try {
    // LLM settings are managed on the LLM page (/llm): omit the `llm` key so
    // this save never touches the stored LLM section (the backend deep-merges
    // the payload over the stored config; omitted sections are preserved).
    const payload: Partial<AppConfig> = JSON.parse(JSON.stringify(config))
    delete payload.llm
    await cfg.save(payload)
    commitSnapshot()

    ElNotification({
      title: t('common.success'),
      message: t('config.saved'),
      type: 'success',
      duration: 3000,
    })

    // Flash border animation on each card individually. Template refs on
    // <el-card> resolve to the component instance, not a DOM element, so the
    // root node must be reached via `$el` (calling classList on the instance
    // itself throws and lands in the catch above, showing a bogus error
    // notification after a successful save).
    const cardRefs = [gitPlatformsCardRef, rulesCardRef, advancedCardRef]
    cardRefs.forEach((cardRef) => {
      const el = (cardRef.value as unknown as { $el?: HTMLElement })?.$el
      if (el?.classList) {
        el.classList.add('flash-success')
        setTimeout(() => el.classList.remove('flash-success'), 600)
      }
    })
  } catch {
    ElNotification({
      title: t('common.error'),
      message: t('config.saveFailed'),
      type: 'error',
      duration: 5000,
    })
  }
}

// --- Navigation Guard ---
onBeforeRouteLeave(async (_to, _from, next) => {
  if (isEditing.value && dirty.value) {
    try {
      await ElMessageBox.confirm(
        t('config.unsaved.discardConfirm'),
        t('config.unsaved.title'),
        {
          confirmButtonText: t('config.unsaved.discard'),
          cancelButtonText: t('config.unsaved.stay'),
          type: 'warning',
        }
      )
      next()
    } catch {
      next(false)
    }
  } else {
    next()
  }
})

// --- Before unload ---
function handleBeforeUnload(e: BeforeUnloadEvent) {
  if (isEditing.value && dirty.value) {
    e.preventDefault()
    e.returnValue = ''
  }
}

// --- Resize handler ---
function handleResize() {
  windowWidth.value = window.innerWidth
}

// --- Lifecycle ---
onMounted(() => {
  window.addEventListener('beforeunload', handleBeforeUnload)
  window.addEventListener('resize', handleResize)
  loadConfig()
  loadStorageBackend()
})

// --- Error handling ---
watch(() => cfg.error.value, (err) => {
  if (err) {
    ElNotification({
      title: t('common.error'),
      message: err,
      type: 'error',
      duration: 5000,
    })
  }
})

onUnmounted(() => {
  window.removeEventListener('beforeunload', handleBeforeUnload)
  window.removeEventListener('resize', handleResize)
})
</script>

<style scoped>
.config-page {
  max-width: 1400px;
  margin: 0 auto;
  animation: pageEnter 0.2s ease;
}

@keyframes pageEnter {
  from {
    opacity: 0;
    transform: translateY(6px);
  }
  to {
    opacity: 1;
    transform: translateY(0);
  }
}

.page-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 24px;
  flex-wrap: wrap;
  gap: 12px;
}

.header-left {
  flex: 1;
  min-width: 0;
}

.page-title {
  font-size: 24px;
  font-weight: 600;
  letter-spacing: -0.02em;
  line-height: 1.3;
  color: var(--text-primary);
  margin-bottom: 4px;
}

.page-subtitle {
  font-size: 14px;
  color: var(--text-secondary);
}

.header-actions {
  display: flex;
  gap: 10px;
  align-items: center;
}

.header-actions .el-button {
  display: flex;
  align-items: center;
  gap: 6px;
}

/* Wrapper lets the tooltip hover target survive the disabled save button */
.save-button-wrapper {
  display: inline-flex;
}

/* Skeleton */
.skeleton-container {
  display: flex;
  flex-direction: column;
  gap: 20px;
}

.skeleton-card {
  padding: 16px;
}

/* Form */
.config-form {
  display: flex;
  flex-direction: column;
  gap: 20px;
}

/* Card Design System */
.config-card {
  background-color: var(--bg-card);
  border: 1px solid var(--border-color);
  border-radius: var(--radius-md);
  box-shadow: var(--shadow-card);
  transition: opacity 0.15s ease, border-color 0.2s ease, box-shadow 0.2s ease;
}

.config-card:hover {
  border-color: var(--brand);
  box-shadow: 0 0 0 1px var(--brand), var(--shadow-card);
}

.config-card :deep(.el-card__header) {
  padding: 14px 20px;
  border-bottom: 1px solid var(--border-color);
}

.card-header {
  display: flex;
  align-items: center;
  gap: 8px;
  font-weight: 500;
  font-size: 14px;
  color: var(--text-primary);
}

.card-body {
  padding: 20px;
}

/* Form label override */
.config-card :deep(.el-form-item__label) {
  font-size: 12px;
}

/* Safety cap so very long i18n labels don't blow out the auto label width */
.config-form :deep(.el-form-item__label) {
  max-width: 220px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

/* Slider with value */
.slider-with-value {
  display: flex;
  align-items: center;
  gap: 12px;
  width: 100%;
}

.slider-with-value .el-slider {
  flex: 1;
}

.slider-value {
  font-size: 14px;
  font-weight: 500;
  color: var(--text-primary);
  min-width: 32px;
  text-align: right;
  font-family: var(--font-mono);
}

/* Disabled slider — override Element Plus bare-dot default */
.slider-with-value :deep(.el-slider.is-disabled) {
  cursor: default;
}
.slider-with-value :deep(.el-slider.is-disabled .el-slider__runway) {
  background-color: var(--border-color);
  cursor: default;
}
.slider-with-value :deep(.el-slider.is-disabled .el-slider__bar) {
  background-color: var(--primary);
  opacity: 0.5;
}
.slider-with-value :deep(.el-slider.is-disabled .el-slider__button) {
  border-color: var(--primary);
  opacity: 0.7;
  width: 14px;
  height: 14px;
}
.slider-with-value :deep(.el-slider.is-disabled .el-slider__button-wrapper) {
  cursor: default;
}
.slider-with-value :deep(.el-slider.is-disabled .el-slider__stop) {
  display: none;
}

/* Tag input */
.tag-input {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
  width: 100%;
  padding: 4px;
  min-height: 32px;
  background: var(--bg-surface);
  border: 1px solid var(--border-color);
  border-radius: var(--radius-sm);
}

.tag-input .el-tag {
  margin: 0;
}

.tag-input .el-input {
  width: 120px;
}

.tag-input .el-button {
  height: 24px;
  padding: 0 8px;
}

/* Advanced toggle */
.advanced-toggle {
  display: flex;
  justify-content: center;
  padding: 8px 0;
}

/* Checkbox group */
:deep(.el-checkbox-group) {
  display: flex;
  flex-wrap: wrap;
  gap: 16px;
}

:deep(.el-checkbox) {
  color: var(--text-primary);
}

/* Flash animation */
@keyframes flashBorder {
  0% {
    border-color: var(--border-color);
  }
  50% {
    border-color: var(--success);
    box-shadow: 0 0 0 2px rgba(34, 197, 94, 0.2);
  }
  100% {
    border-color: var(--border-color);
  }
}

.config-card.flash-success {
  animation: flashBorder 0.6s ease;
}

/* Shake animation for validation errors */
@keyframes shake {
  0%, 100% { transform: translateX(0); }
  25% { transform: translateX(-4px); }
  75% { transform: translateX(4px); }
}

.shake-error {
  animation: shake 0.3s ease-in-out;
}

/* Mobile sticky actions */
.mobile-actions {
  display: none;
  position: fixed;
  bottom: 0;
  left: 0;
  right: 0;
  padding: 12px 16px;
  background: var(--bg-surface);
  border-top: 1px solid var(--border-color);
  gap: 12px;
  justify-content: flex-end;
  z-index: 50;
}

.mobile-badge :deep(.el-badge__content) {
  top: 4px;
  right: 4px;
}

/* Responsive */
@media (max-width: 767px) {
  .header-actions {
    display: none;
  }

  .mobile-actions {
    display: flex;
  }

  .page-header {
    flex-direction: column;
    align-items: flex-start;
  }

  .config-page {
    padding: 0;
  }

  .card-body {
    padding: 16px;
  }

  :deep(.el-form-item__label) {
    font-size: 13px;
  }

  :deep(.el-slider) {
    width: 100%;
  }
}

@media (max-width: 1023px) {
  .config-page {
    max-width: 100%;
  }
}

/* Transitions for edit mode buttons */
.header-actions .el-button {
  transition: all 0.15s ease;
}

/* Custom scrollbar for cards */
.config-card :deep(.el-card__body) {
  max-height: none;
  overflow: visible;
}
</style>
