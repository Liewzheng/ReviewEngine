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
      class="config-form"
      @submit.prevent
    >
      <!-- GitLab Card -->
      <el-card ref="gitlabCardRef" class="config-card">
        <template #header>
          <div class="card-header">
            <el-icon><Link /></el-icon>
            <span>{{ $t('config.gitlab.title') }}</span>
          </div>
        </template>
        <div class="card-body">
          <el-row :gutter="20">
            <el-col :xs="24" :sm="12">
              <el-form-item :label="$t('config.gitlab.url')" prop="gitlab.url">
                <el-input v-model="config.gitlab.url" :disabled="!isEditing" :placeholder="$t('config.gitlab.urlPlaceholder')" />
              </el-form-item>
            </el-col>
            <el-col :xs="24" :sm="12">
              <el-form-item :label="$t('header.apiToken')" prop="gitlab.apiToken">
                <div v-if="!isEditing" class="readonly-field">
                  <template v-if="!config.gitlab.apiToken">
                    <span class="empty-text">{{ $t('config.notSet') }}</span>
                  </template>
                  <template v-else-if="!revealed.apiToken">
                    <span class="masked-text">••••••••••••</span>
                    <el-button size="small" :aria-label="$t('config.gitlab.revealApiTokenAria')" @click.stop="revealField('apiToken')">
                      <el-icon><View /></el-icon>
                    </el-button>
                  </template>
                  <template v-else>
                    <span class="revealed-value">{{ config.gitlab.apiToken }}</span>
                    <span class="countdown">{{ $t('config.revealCountdown', { count: revealCountdown.apiToken }) }}</span>
                  </template>
                </div>
                <el-input v-else v-model="config.gitlab.apiToken" :disabled="!isEditing" show-password :placeholder="$t('config.gitlab.apiTokenPlaceholder')" />
              </el-form-item>
            </el-col>
            <el-col :xs="24" :sm="12">
              <el-form-item :label="$t('config.gitlab.webhookSecret')" prop="gitlab.webhookSecret">
                <div v-if="!isEditing" class="readonly-field">
                  <template v-if="!config.gitlab.webhookSecret">
                    <span class="empty-text">{{ $t('config.notSet') }}</span>
                  </template>
                  <template v-else-if="!revealed.webhookSecret">
                    <span class="masked-text">••••••••••••</span>
                    <el-button size="small" :aria-label="$t('config.gitlab.revealWebhookAria')" @click.stop="revealField('webhookSecret')">
                      <el-icon><View /></el-icon>
                    </el-button>
                  </template>
                  <template v-else>
                    <span class="revealed-value">{{ config.gitlab.webhookSecret }}</span>
                    <span class="countdown">{{ $t('config.revealCountdown', { count: revealCountdown.webhookSecret }) }}</span>
                  </template>
                </div>
                <el-input v-else v-model="config.gitlab.webhookSecret" :disabled="!isEditing" show-password :placeholder="$t('common.optional')" />
              </el-form-item>
            </el-col>
            <el-col :xs="24" :sm="12">
              <el-form-item :label="$t('config.gitlab.webhookSigningSecret')" prop="gitlab.webhookSigningSecret">
                <div v-if="!isEditing" class="readonly-field">
                  <template v-if="!config.gitlab.webhookSigningSecret">
                    <span class="empty-text">{{ $t('config.notSet') }}</span>
                  </template>
                  <template v-else-if="!revealed.webhookSigningSecret">
                    <span class="masked-text">••••••••••••</span>
                    <el-button size="small" :aria-label="$t('config.gitlab.revealSigningAria')" @click.stop="revealField('webhookSigningSecret')">
                      <el-icon><View /></el-icon>
                    </el-button>
                  </template>
                  <template v-else>
                    <span class="revealed-value">{{ config.gitlab.webhookSigningSecret }}</span>
                    <span class="countdown">{{ $t('config.revealCountdown', { count: revealCountdown.webhookSigningSecret }) }}</span>
                  </template>
                </div>
                <el-input v-else v-model="config.gitlab.webhookSigningSecret" :disabled="!isEditing" show-password :placeholder="$t('config.gitlab.signingPlaceholder')" />
                <div v-if="isEditing" class="form-item-help">{{ $t('config.gitlab.signingHelp') }}</div>
              </el-form-item>
            </el-col>
            <el-col :xs="24" :sm="12">
              <el-form-item :label="$t('config.gitlab.defaultProject')" prop="gitlab.defaultProject">
                <el-input v-model="config.gitlab.defaultProject" :disabled="!isEditing" clearable :placeholder="$t('config.gitlab.defaultProjectPlaceholder')" />
                <div v-if="isEditing" class="form-item-help">{{ $t('config.gitlab.defaultProjectHelp') }}</div>
              </el-form-item>
            </el-col>
            <el-col :xs="24" :sm="12">
              <el-form-item :label="$t('config.gitlab.mrLabel')" prop="gitlab.mrLabel">
                <el-input v-model="config.gitlab.mrLabel" :disabled="!isEditing" placeholder="needs-review" />
              </el-form-item>
            </el-col>
            <el-col :xs="24" :sm="12">
              <el-form-item :label="$t('config.gitlab.autoReview')" prop="gitlab.autoReview">
                <el-switch v-model="config.gitlab.autoReview" :disabled="!isEditing" />
              </el-form-item>
            </el-col>
          </el-row>
        </div>
      </el-card>

      <!-- LLM Card -->
      <el-card ref="llmCardRef" class="config-card">
        <template #header>
          <div class="card-header">
            <el-icon><Cpu /></el-icon>
            <span>{{ $t('config.llm.title') }}</span>
          </div>
        </template>
        <div class="card-body">
          <el-row :gutter="20">
            <el-col :xs="24" :sm="12">
              <el-form-item :label="$t('config.llm.apiBaseUrl')" prop="llm.apiBaseUrl">
                <el-input v-model="config.llm.apiBaseUrl" :disabled="!isEditing" :placeholder="$t('config.llm.apiBasePlaceholder')" />
              </el-form-item>
            </el-col>
            <el-col :xs="24" :sm="12">
              <el-form-item :label="$t('config.llm.apiKey')" prop="llm.openaiApiKey">
                <el-input v-model="config.llm.openaiApiKey" :disabled="!isEditing" show-password :placeholder="$t('config.llm.apiKeyPlaceholder')" />
              </el-form-item>
            </el-col>
            <el-col :xs="24" :sm="12">
              <el-form-item :label="$t('config.llm.defaultModel')" prop="llm.defaultModel">
                <el-select
                  v-model="config.llm.defaultModel"
                  :disabled="!isEditing"
                  :loading="modelFetchLoading"
                  :placeholder="$t('config.llm.selectModelPlaceholder')"
                  style="width: 100%"
                >
                  <el-option
                    v-for="model in availableModels"
                    :key="model"
                    :label="model"
                    :value="model"
                  />
                </el-select>
                <div v-if="modelFetchError" class="form-item-help error-text">{{ modelFetchError }}</div>
              </el-form-item>
            </el-col>
            <el-col :xs="24" :sm="12">
              <el-form-item :label="$t('config.llm.maxTokens')" prop="llm.maxTokens">
                <el-input-number v-model="config.llm.maxTokens" :disabled="!isEditing" :min="128" :max="8192" :step="128" style="width: 100%" />
              </el-form-item>
            </el-col>
            <el-col :xs="24" :sm="12">
              <el-form-item :label="$t('config.llm.temperature')" prop="llm.temperature">
                <div class="slider-with-value">
                  <el-slider v-model="config.llm.temperature" :disabled="!isEditing" :min="0" :max="2" :step="0.1" />
                  <span class="slider-value">{{ config.llm.temperature }}</span>
                </div>
              </el-form-item>
            </el-col>
            <el-col :xs="24" :sm="12">
              <el-form-item :label="$t('config.llm.timeout')" prop="llm.timeoutSeconds">
                <el-input-number v-model="config.llm.timeoutSeconds" :disabled="!isEditing" :min="5" :max="300" :step="5" style="width: 100%" />
              </el-form-item>
            </el-col>
            <el-col :xs="24" :sm="12">
              <el-form-item :label="$t('config.llm.retryAttempts')" prop="llm.retryAttempts">
                <el-input-number v-model="config.llm.retryAttempts" :disabled="!isEditing" :min="0" :max="5" style="width: 100%" />
              </el-form-item>
            </el-col>
          </el-row>
          <div class="test-connection">
            <el-button :loading="testingConnection" @click="testConnection">
              <el-icon><Connection /></el-icon>
              <span>{{ $t('common.testConnection') }}</span>
            </el-button>
            <el-tag v-if="testResult" :type="testResult.success ? 'success' : 'danger'" effect="dark">
              {{ testResult.success ? $t('config.llm.connected', { n: testResult.latencyMs }) : $t('config.llm.testFailed', { error: testResult.error }) }}
            </el-tag>
          </div>
        </div>
      </el-card>

      <!-- Additional LLM Providers Card -->
      <ProvidersSection
        :providers="additionalProviders"
        :is-editing="isEditing"
        :loading="providersLoading"
        @toggle="toggleProvider"
        @remove="confirmDeleteProvider"
        @add="addProvider"
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
                <el-input-number v-model="config.rules.maxReviewDurationSeconds" :disabled="!isEditing" :min="30" :max="3600" :step="30" style="width: 100%" />
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
        <el-button link type="primary" @click="showAdvanced = !showAdvanced">
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
  Connection,
  Cpu,
  Edit,
  Link,
  Plus,
  Refresh,
  Tools,
  View,
} from '@element-plus/icons-vue'
import { ElMessageBox, ElNotification } from 'element-plus'
import { useI18n } from 'vue-i18n'
import { useConfig } from '../composables/useConfig'
import { useConfigForm } from '../composables/useConfigForm'
import { useProviders } from '../composables/useProviders'
import ProvidersSection from '../components/Config/ProvidersSection.vue'

// --- Composables ---
const { t } = useI18n()
const cfg = useConfig()

const {
  config,
  isEditing,
  formRef,
  configDirty,
  rules,
  revealed,
  revealCountdown,
  revealField,
  availableModels,
  modelFetchLoading,
  modelFetchError,
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
  testConnection,
} = useConfigForm(cfg)

const {
  additionalProviders,
  providersLoading,
  providersDirty,
  loadProviders,
  addProvider,
  toggleProvider,
  confirmDeleteProvider,
  resetProviders,
  saveAdditionalProviders,
  saveProvidersOnly,
} = useProviders(isEditing, configDirty)

// --- State ---
const loading = cfg.loading
const loadError = computed(() => !!cfg.error.value)
const saving = cfg.saving
const testingConnection = cfg.testing
const testResult = cfg.testResult
const showAdvanced = ref(false)

// Card refs for flash animation
const gitlabCardRef = ref<HTMLElement>()
const llmCardRef = ref<HTMLElement>()
const rulesCardRef = ref<HTMLElement>()
const advancedCardRef = ref<HTMLElement>()

// Responsive layout
const windowWidth = ref(window.innerWidth)
const labelPosition = computed(() => (windowWidth.value >= 1024 ? 'left' : 'top'))

// --- Computed ---
const dirty = computed(() => configDirty.value || providersDirty.value)

// --- Methods ---
function cancelEdit() {
  restoreSnapshot()
  // Discard unsaved provider edits as well, otherwise they would keep the
  // page dirty the next time edit mode is entered.
  resetProviders()
}

async function saveChanges() {
  // Provider-only changes are independent of the main form: skip main-form
  // validation, which would otherwise block provider management when the
  // main config is incomplete (e.g. empty demo GitLab URL/token).
  if (!configDirty.value && providersDirty.value) {
    await saveProvidersOnly()
    return
  }
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
    await cfg.save(JSON.parse(JSON.stringify(config)))
    await saveAdditionalProviders()
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
    const cardRefs = [gitlabCardRef, llmCardRef, rulesCardRef, advancedCardRef]
    cardRefs.forEach((cardRef) => {
      const el = (cardRef.value as unknown as { $el?: HTMLElement })?.$el
      if (el?.classList) {
        el.classList.add('flash-success')
        setTimeout(() => el.classList.remove('flash-success'), 600)
      }
    })
  } catch (e) {
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
  loadProviders()
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
  max-width: 900px;
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

/* Readonly fields */
.readonly-field {
  display: flex;
  align-items: center;
  gap: 10px;
  height: 32px;
  padding: 0 12px;
  background: var(--bg-surface);
  border: 1px solid var(--border-color);
  border-radius: var(--radius-sm);
  font-size: 14px;
}

.masked-text {
  color: var(--text-secondary);
  font-family: var(--font-mono);
  letter-spacing: 2px;
  flex: 1;
}

.revealed-value {
  color: var(--text-primary);
  font-family: var(--font-mono);
  font-size: 13px;
  flex: 1;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.countdown {
  font-size: 12px;
  color: var(--warning);
  white-space: nowrap;
}

.empty-text {
  color: var(--text-secondary);
  font-style: italic;
  flex: 1;
}

/* Helper text below form inputs */
.form-item-help {
  font-size: 12px;
  color: var(--text-secondary);
  margin-top: 6px;
  line-height: 1.4;
}

.form-item-help.error-text {
  color: var(--danger);
}

/* Slider with value */
.slider-with-value {
  display: flex;
  align-items: center;
  gap: 12px;
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

/* Test connection */
.test-connection {
  display: flex;
  align-items: center;
  justify-content: flex-end;
  gap: 12px;
  margin-top: 16px;
  padding-top: 16px;
  border-top: 1px solid var(--border-color);
}

/* Tag input */
.tag-input {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
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
