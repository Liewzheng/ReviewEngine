<template>
  <div class="config-page">
    <!-- Page Header -->
    <div class="page-header">
      <div class="header-left">
        <h2 class="page-title">Configuration</h2>
        <p class="page-subtitle">
          {{ isEditing ? 'Edit mode — remember to save your changes' : 'Manage Review-Engine settings' }}
        </p>
      </div>
      <div class="header-actions">
        <template v-if="!isEditing">
          <el-button type="primary" @click="enterEditMode">
            <el-icon><Edit /></el-icon>
            <span>Edit Configuration</span>
          </el-button>
          <el-button @click="refreshConfig">
            <el-icon><Refresh /></el-icon>
            <span>Refresh</span>
          </el-button>
        </template>
        <template v-else>
          <el-badge :is-dot="dirty" type="danger">
            <el-button type="primary" :loading="saving" :disabled="!dirty || !formValid" @click="saveChanges">
              <el-icon><Check /></el-icon>
              <span>Save Changes</span>
            </el-button>
          </el-badge>
          <el-button @click="cancelEdit">
            <el-icon><Close /></el-icon>
            <span>Cancel</span>
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
    <el-empty v-else-if="loadError" description="Failed to load configuration" />

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
            <span>GitLab Integration</span>
          </div>
        </template>
        <div class="card-body">
          <el-row :gutter="20">
            <el-col :xs="24" :sm="12">
              <el-form-item label="GitLab URL" prop="gitlab.url">
                <el-input v-model="config.gitlab.url" :disabled="!isEditing" placeholder="https://gitlab.example.com" />
              </el-form-item>
            </el-col>
            <el-col :xs="24" :sm="12">
              <el-form-item label="API Token" prop="gitlab.apiToken">
                <div v-if="!isEditing" class="readonly-field">
                  <template v-if="!revealed.apiToken">
                    <span class="masked-text">••••••••••••</span>
                    <el-button size="small" aria-label="Reveal API Token" @click.stop="revealField('apiToken')">
                      <el-icon><View /></el-icon>
                    </el-button>
                  </template>
                  <template v-else>
                    <span class="revealed-value">{{ config.gitlab.apiToken }}</span>
                    <span class="countdown">Visible for {{ revealCountdown.apiToken }}s...</span>
                  </template>
                </div>
                <el-input v-else v-model="config.gitlab.apiToken" :disabled="!isEditing" show-password placeholder="glpat-xxxxxxxxxxxxxxxxxxxx" />
              </el-form-item>
            </el-col>
            <el-col :xs="24" :sm="12">
              <el-form-item label="Webhook Secret" prop="gitlab.webhookSecret">
                <div v-if="!isEditing" class="readonly-field">
                  <template v-if="!revealed.webhookSecret">
                    <span class="masked-text">••••••••••••</span>
                    <el-button size="small" aria-label="Reveal Webhook Secret" @click.stop="revealField('webhookSecret')">
                      <el-icon><View /></el-icon>
                    </el-button>
                  </template>
                  <template v-else>
                    <span class="revealed-value">{{ config.gitlab.webhookSecret }}</span>
                    <span class="countdown">Visible for {{ revealCountdown.webhookSecret }}s...</span>
                  </template>
                </div>
                <el-input v-else v-model="config.gitlab.webhookSecret" :disabled="!isEditing" show-password placeholder="Optional" />
              </el-form-item>
            </el-col>
            <el-col :xs="24" :sm="12">
              <el-form-item label="Webhook Signing Secret" prop="gitlab.webhookSigningSecret">
                <div v-if="!isEditing" class="readonly-field">
                  <template v-if="!revealed.webhookSigningSecret">
                    <span class="masked-text">••••••••••••</span>
                    <el-button size="small" aria-label="Reveal Webhook Signing Secret" @click.stop="revealField('webhookSigningSecret')">
                      <el-icon><View /></el-icon>
                    </el-button>
                  </template>
                  <template v-else>
                    <span class="revealed-value">{{ config.gitlab.webhookSigningSecret }}</span>
                    <span class="countdown">Visible for {{ revealCountdown.webhookSigningSecret }}s...</span>
                  </template>
                </div>
                <el-input v-else v-model="config.gitlab.webhookSigningSecret" :disabled="!isEditing" show-password placeholder="GitLab 19.0+ signing token" />
              </el-form-item>
            </el-col>
            <el-col :xs="24" :sm="12">
              <el-form-item label="Default Project" prop="gitlab.defaultProject">
                <el-select v-model="config.gitlab.defaultProject" :disabled="!isEditing" placeholder="Select a project" clearable style="width: 100%">
                  <el-option label="my-group/my-project" value="my-group/my-project" />
                  <el-option label="acme/frontend" value="acme/frontend" />
                  <el-option label="acme/backend" value="acme/backend" />
                  <el-option label="infra/terraform" value="infra/terraform" />
                </el-select>
              </el-form-item>
            </el-col>
            <el-col :xs="24" :sm="12">
              <el-form-item label="Merge Request Label" prop="gitlab.mrLabel">
                <el-input v-model="config.gitlab.mrLabel" :disabled="!isEditing" placeholder="needs-review" />
              </el-form-item>
            </el-col>
            <el-col :xs="24" :sm="12">
              <el-form-item label="Auto-review enabled" prop="gitlab.autoReview">
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
            <span>LLM Settings</span>
          </div>
        </template>
        <div class="card-body">
          <el-row :gutter="20">
            <el-col :xs="24" :sm="12">
              <el-form-item label="Primary Provider" prop="llm.primaryProvider">
                <el-select v-model="config.llm.primaryProvider" :disabled="!isEditing" placeholder="Select provider" style="width: 100%">
                  <el-option label="OpenAI" value="openai" />
                  <el-option label="Anthropic" value="anthropic" />
                  <el-option label="Ollama (Local)" value="ollama" />
                </el-select>
              </el-form-item>
            </el-col>
            <el-col :xs="24" :sm="12">
              <el-form-item label="Default Model" prop="llm.defaultModel">
                <el-select v-model="config.llm.defaultModel" :disabled="!isEditing" placeholder="Select model" style="width: 100%">
                  <el-option
                    v-for="model in availableModels"
                    :key="model"
                    :label="model"
                    :value="model"
                  />
                </el-select>
              </el-form-item>
            </el-col>
            <el-col :xs="24" :sm="12" v-show="config.llm.primaryProvider === 'openai'">
              <el-form-item label="OpenAI API Key" prop="llm.openaiApiKey">
                <el-input v-model="config.llm.openaiApiKey" :disabled="!isEditing" show-password placeholder="sk-..." />
              </el-form-item>
            </el-col>
            <el-col :xs="24" :sm="12" v-show="config.llm.primaryProvider === 'anthropic'">
              <el-form-item label="Anthropic API Key" prop="llm.anthropicApiKey">
                <el-input v-model="config.llm.anthropicApiKey" :disabled="!isEditing" show-password placeholder="sk-ant-..." />
              </el-form-item>
            </el-col>
            <el-col :xs="24" :sm="12" v-show="config.llm.primaryProvider === 'ollama'">
              <el-form-item label="Local Ollama URL" prop="llm.ollamaUrl">
                <el-input v-model="config.llm.ollamaUrl" :disabled="!isEditing" placeholder="http://localhost:11434" />
              </el-form-item>
            </el-col>
            <el-col :xs="24" :sm="12">
              <el-form-item label="Max Tokens" prop="llm.maxTokens">
                <el-input-number v-model="config.llm.maxTokens" :disabled="!isEditing" :min="128" :max="8192" :step="128" style="width: 100%" />
              </el-form-item>
            </el-col>
            <el-col :xs="24" :sm="12">
              <el-form-item label="Temperature" prop="llm.temperature">
                <div class="slider-with-value">
                  <el-slider v-model="config.llm.temperature" :disabled="!isEditing" :min="0" :max="2" :step="0.1" />
                  <span class="slider-value">{{ config.llm.temperature }}</span>
                </div>
              </el-form-item>
            </el-col>
            <el-col :xs="24" :sm="12">
              <el-form-item label="Timeout (seconds)" prop="llm.timeoutSeconds">
                <el-input-number v-model="config.llm.timeoutSeconds" :disabled="!isEditing" :min="5" :max="300" :step="5" style="width: 100%" />
              </el-form-item>
            </el-col>
            <el-col :xs="24" :sm="12">
              <el-form-item label="Retry Attempts" prop="llm.retryAttempts">
                <el-input-number v-model="config.llm.retryAttempts" :disabled="!isEditing" :min="0" :max="5" style="width: 100%" />
              </el-form-item>
            </el-col>
          </el-row>
          <div class="test-connection">
            <el-button :loading="testingConnection" @click="testConnection">
              <el-icon><Connection /></el-icon>
              <span>Test Connection</span>
            </el-button>
            <el-tag v-if="testResult" :type="testResult.success ? 'success' : 'danger'" effect="dark">
              {{ testResult.success ? `Connected — ${testResult.latencyMs}ms` : `Failed — ${testResult.error}` }}
            </el-tag>
          </div>
        </div>
      </el-card>

      <!-- Review Rules Card -->
      <el-card ref="rulesCardRef" class="config-card">
        <template #header>
          <div class="card-header">
            <el-icon><Collection /></el-icon>
            <span>Review Rules</span>
          </div>
        </template>
        <div class="card-body">
          <el-row :gutter="20">
            <el-col :xs="24" :sm="12">
              <el-form-item label="Minimum review score" prop="rules.minScore">
                <div class="slider-with-value">
                  <el-slider v-model="config.rules.minScore" :disabled="!isEditing" :min="0" :max="100" :step="5" />
                  <span class="slider-value">{{ config.rules.minScore }}</span>
                </div>
              </el-form-item>
            </el-col>
            <el-col :xs="24" :sm="12">
              <el-form-item label="Max review duration (seconds)" prop="rules.maxReviewDurationSeconds">
                <el-input-number v-model="config.rules.maxReviewDurationSeconds" :disabled="!isEditing" :min="30" :max="3600" :step="30" style="width: 100%" />
              </el-form-item>
            </el-col>
            <el-col :xs="24" :sm="12">
              <el-form-item label="Block MR on critical" prop="rules.blockOnCritical">
                <el-switch v-model="config.rules.blockOnCritical" :disabled="!isEditing" />
              </el-form-item>
            </el-col>
            <el-col :xs="24" :sm="12">
              <el-form-item label="Auto-comment on pass" prop="rules.autoCommentOnPass">
                <el-switch v-model="config.rules.autoCommentOnPass" :disabled="!isEditing" />
              </el-form-item>
            </el-col>
            <el-col :xs="24">
              <el-form-item label="Comment template" prop="rules.commentTemplate">
                <el-input
                  v-model="config.rules.commentTemplate"
                  :disabled="!isEditing"
                  type="textarea"
                  :rows="4"
                  :maxlength="2000"
                  show-word-limit
                  placeholder="Code review completed. Overall score: {{score}}/100. {{summary}}"
                />
              </el-form-item>
            </el-col>
            <el-col :xs="24">
              <el-form-item label="Excluded file patterns" prop="rules.excludedPatterns">
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
                    ref="patternInputRef"
                    v-model="patternInputValue"
                    size="small"
                    @keyup.enter="addPattern"
                    @blur="addPattern"
                  />
                  <el-button v-else size="small" @click="showPatternInput">
                    <el-icon><Plus /></el-icon>
                    Add Pattern
                  </el-button>
                </div>
              </el-form-item>
            </el-col>
            <el-col :xs="24">
              <el-form-item label="Required experts" prop="rules.requiredExperts">
                <el-checkbox-group v-model="config.rules.requiredExperts" :disabled="!isEditing">
                  <el-checkbox label="Security" />
                  <el-checkbox label="Performance" />
                  <el-checkbox label="Quality" />
                  <el-checkbox label="Maintainability" />
                  <el-checkbox label="Test Coverage" />
                  <el-checkbox label="Documentation" />
                  <el-checkbox label="Dependencies" />
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
          {{ showAdvanced ? 'Hide Advanced' : 'Show Advanced' }}
        </el-button>
      </div>

      <!-- Advanced Card -->
      <el-card v-show="showAdvanced" ref="advancedCardRef" class="config-card">
        <template #header>
          <div class="card-header">
            <el-icon><Tools /></el-icon>
            <span>Advanced Options</span>
          </div>
        </template>
        <div class="card-body">
          <el-row :gutter="20">
            <el-col :xs="24" :sm="12">
              <el-form-item label="Log level" prop="advanced.logLevel">
                <el-select v-model="config.advanced.logLevel" :disabled="!isEditing" style="width: 100%">
                  <el-option label="Debug" value="debug" />
                  <el-option label="Info" value="info" />
                  <el-option label="Warn" value="warn" />
                  <el-option label="Error" value="error" />
                </el-select>
              </el-form-item>
            </el-col>
            <el-col :xs="24" :sm="12">
              <el-form-item label="Log retention (days)" prop="advanced.logRetentionDays">
                <el-input-number v-model="config.advanced.logRetentionDays" :disabled="!isEditing" :min="1" :max="90" style="width: 100%" />
              </el-form-item>
            </el-col>
            <el-col :xs="24" :sm="12">
              <el-form-item label="SSE heartbeat interval (seconds)" prop="advanced.sseHeartbeatInterval">
                <el-input-number v-model="config.advanced.sseHeartbeatInterval" :disabled="!isEditing" :min="5" :max="60" style="width: 100%" />
              </el-form-item>
            </el-col>
            <el-col :xs="24" :sm="12">
              <el-form-item label="Max concurrent reviews" prop="advanced.maxConcurrentReviews">
                <el-input-number v-model="config.advanced.maxConcurrentReviews" :disabled="!isEditing" :min="1" :max="20" style="width: 100%" />
              </el-form-item>
            </el-col>
            <el-col :xs="24" :sm="12">
              <el-form-item label="Request timeout (seconds)" prop="advanced.requestTimeout">
                <el-input-number v-model="config.advanced.requestTimeout" :disabled="!isEditing" :min="10" :max="300" style="width: 100%" />
              </el-form-item>
            </el-col>
            <el-col :xs="24" :sm="12">
              <el-form-item label="Enable metrics" prop="advanced.enableMetrics">
                <el-switch v-model="config.advanced.enableMetrics" :disabled="!isEditing" />
              </el-form-item>
            </el-col>
            <el-col :xs="24" :sm="12">
              <el-form-item label="Debug mode" prop="advanced.debugMode">
                <el-switch v-model="config.advanced.debugMode" :disabled="!isEditing" />
              </el-form-item>
            </el-col>
          </el-row>
        </div>
      </el-card>
    </el-form>

    <!-- Mobile Sticky Actions -->
    <div v-if="isEditing" class="mobile-actions">
      <el-badge :is-dot="dirty" type="danger" class="mobile-badge">
        <el-button type="primary" :loading="saving" :disabled="!dirty || !formValid" @click="saveChanges">
          Save Changes
        </el-button>
      </el-badge>
      <el-button @click="cancelEdit">Cancel</el-button>
    </div>
  </div>
</template>

<script setup lang="ts">
import { ref, computed, reactive, watch, onMounted, onUnmounted, nextTick } from 'vue'
import { onBeforeRouteLeave } from 'vue-router'
import { ElMessageBox, ElNotification, type FormInstance, type FormRules } from 'element-plus'
import { useConfig } from '../composables/useConfig'
import { type AppConfig, providerModels } from '../types/config'

// --- Composable ---
const cfg = useConfig()

// --- State ---
const loading = cfg.loading
const loadError = computed(() => !!cfg.error.value)
const isEditing = ref(false)
const saving = cfg.saving
const testingConnection = cfg.testing
const testResult = cfg.testResult
const formValid = ref(true)
const showAdvanced = ref(false)
const formRef = ref<FormInstance>()

const defaultConfig: AppConfig = {
  gitlab: { url: '', apiToken: '', webhookSecret: '', webhookSigningSecret: '', defaultProject: '', mrLabel: '', autoReview: false },
  llm: { primaryProvider: 'openai', openaiApiKey: '', anthropicApiKey: '', ollamaUrl: '', defaultModel: '', maxTokens: 4096, temperature: 0.7, timeoutSeconds: 60, retryAttempts: 3 },
  rules: { minScore: 75, blockOnCritical: true, autoCommentOnPass: true, commentTemplate: '', excludedPatterns: [], requiredExperts: [], maxReviewDurationSeconds: 300 },
  advanced: { logLevel: 'info', logRetentionDays: 30, sseHeartbeatInterval: 15, maxConcurrentReviews: 5, requestTimeout: 120, enableMetrics: true, debugMode: false },
}

const config = reactive<AppConfig>(defaultConfig)
const originalConfig = ref<AppConfig | null>(null)

// Card refs for flash animation
const gitlabCardRef = ref<HTMLElement>()
const llmCardRef = ref<HTMLElement>()
const rulesCardRef = ref<HTMLElement>()
const advancedCardRef = ref<HTMLElement>()

// Reveal state for read-only mode
const revealed = reactive({
  apiToken: false,
  webhookSecret: false,
  webhookSigningSecret: false,
})
const revealCountdown = reactive({
  apiToken: 0,
  webhookSecret: 0,
  webhookSigningSecret: 0,
})
const revealTimers = reactive<Record<string, number>>({})

// Tag input state
const patternInputVisible = ref(false)
const patternInputValue = ref('')
const patternInputRef = ref<any>()

// Responsive layout
const windowWidth = ref(window.innerWidth)
const labelPosition = computed(() => (windowWidth.value >= 1024 ? 'left' : 'top'))

// --- Computed ---
const availableModels = computed(() => {
  return providerModels[config.llm.primaryProvider] || []
})

const dirty = computed(() => {
  if (!isEditing.value || !originalConfig.value) return false
  return JSON.stringify(config) !== JSON.stringify(originalConfig.value)
})

// --- Validation ---
function validateUrl(_rule: any, value: string, callback: Function) {
  try {
    new URL(value)
    callback()
  } catch {
    callback(new Error('Please enter a valid URL'))
  }
}

const rules = computed<FormRules>(() => ({
  'gitlab.url': [
    { required: true, message: 'GitLab URL is required', trigger: 'blur' },
    { validator: validateUrl, trigger: 'blur' },
  ],
  'gitlab.apiToken': [
    { required: true, message: 'API Token is required', trigger: 'blur' },
    { min: 10, message: 'API Token must be at least 10 characters', trigger: 'blur' },
  ],
  'llm.primaryProvider': [
    { required: true, message: 'Primary Provider is required', trigger: 'change' },
  ],
  'llm.openaiApiKey': [
    {
      validator: (_rule: any, value: any, callback: any) => {
        if (config.llm.primaryProvider === 'openai' && !value) {
          callback(new Error('OpenAI API Key is required'))
        } else {
          callback()
        }
      },
      trigger: 'blur',
    },
  ],
  'llm.anthropicApiKey': [
    {
      validator: (_rule: any, value: any, callback: any) => {
        if (config.llm.primaryProvider === 'anthropic' && !value) {
          callback(new Error('Anthropic API Key is required'))
        } else {
          callback()
        }
      },
      trigger: 'blur',
    },
  ],
  'llm.ollamaUrl': [
    {
      validator: (_rule: any, value: any, callback: any) => {
        if (config.llm.primaryProvider === 'ollama') {
          if (!value) {
            callback(new Error('Ollama URL is required'))
          } else {
            try {
              new URL(value)
              callback()
            } catch {
              callback(new Error('Please enter a valid URL'))
            }
          }
        } else {
          callback()
        }
      },
      trigger: 'blur',
    },
  ],
  'llm.defaultModel': [
    { required: true, message: 'Default Model is required', trigger: 'change' },
  ],
  'rules.requiredExperts': [
    {
      validator: (_rule: any, value: any, callback: any) => {
        if (!value || value.length === 0) {
          callback(new Error('At least one expert is required'))
        } else {
          callback()
        }
      },
      trigger: 'change',
    },
  ],
}))

// --- Watchers ---
watch(config, () => {
  if (isEditing.value && formRef.value) {
    formRef.value.validate((valid: boolean) => {
      formValid.value = valid
    }).catch(() => { formValid.value = false })
  }
}, { deep: true })

watch(() => config.llm.primaryProvider, (newProvider) => {
  const models = providerModels[newProvider] || []
  if (!models.includes(config.llm.defaultModel)) {
    config.llm.defaultModel = models[0] || ''
  }
})

// --- Methods ---
function enterEditMode() {
  originalConfig.value = JSON.parse(JSON.stringify(config))
  isEditing.value = true
  formValid.value = true
}

function cancelEdit() {
  if (originalConfig.value) {
    Object.assign(config, originalConfig.value)
  }
  isEditing.value = false
  formValid.value = true
}

async function saveChanges() {
  if (!formRef.value) return
  const valid = await formRef.value.validate().catch(() => false)
  if (!valid) {
    nextTick(() => {
      const firstError = document.querySelector('.el-form-item.is-error')
      if (firstError) {
        firstError.classList.add('shake-error')
        setTimeout(() => firstError.classList.remove('shake-error'), 300)
        firstError.scrollIntoView({ behavior: 'smooth', block: 'center' })
      }
    })
    ElNotification({
      title: 'Validation Error',
      message: 'Please fix validation errors before saving',
      type: 'warning',
      duration: 3000,
    })
    return
  }

  try {
    await cfg.save(JSON.parse(JSON.stringify(config)))
    originalConfig.value = JSON.parse(JSON.stringify(config))
    isEditing.value = false

    ElNotification({
      title: 'Success',
      message: 'Configuration saved successfully',
      type: 'success',
      duration: 3000,
    })

    // Flash border animation on each card individually
    const cardRefs = [gitlabCardRef, llmCardRef, rulesCardRef, advancedCardRef]
    cardRefs.forEach((cardRef) => {
      const el = cardRef.value
      if (el) {
        el.classList.add('flash-success')
        setTimeout(() => el.classList.remove('flash-success'), 600)
      }
    })
  } catch (e) {
    ElNotification({
      title: 'Error',
      message: 'Failed to save configuration',
      type: 'error',
      duration: 5000,
    })
  }
}

async function refreshConfig() {
  await cfg.fetch()
  if (cfg.config.value) {
    Object.assign(config, cfg.config.value)
  }
  ElNotification({
    title: 'Refreshed',
    message: 'Configuration refreshed',
    type: 'info',
    duration: 2000,
  })
}

async function testConnection() {
  await cfg.test({
    provider: config.llm.primaryProvider,
    model: config.llm.defaultModel,
    apiKey: config.llm.primaryProvider === 'openai' ? config.llm.openaiApiKey : config.llm.anthropicApiKey,
  })
}

function revealField(field: 'apiToken' | 'webhookSecret' | 'webhookSigningSecret') {
  revealed[field] = true
  revealCountdown[field] = 5
  if (revealTimers[field]) clearInterval(revealTimers[field])
  revealTimers[field] = window.setInterval(() => {
    revealCountdown[field]--
    if (revealCountdown[field] <= 0) {
      clearInterval(revealTimers[field])
      revealed[field] = false
      delete revealTimers[field]
    }
  }, 1000)
}

// Pattern tag input
function showPatternInput() {
  patternInputVisible.value = true
  nextTick(() => {
    patternInputRef.value?.focus()
  })
}

function addPattern() {
  const value = patternInputValue.value.trim()
  if (value && !config.rules.excludedPatterns.includes(value)) {
    config.rules.excludedPatterns.push(value)
  }
  patternInputVisible.value = false
  patternInputValue.value = ''
}

function removePattern(index: number) {
  config.rules.excludedPatterns.splice(index, 1)
}

// --- Navigation Guard ---
onBeforeRouteLeave(async (_to, _from, next) => {
  if (isEditing.value && dirty.value) {
    try {
      await ElMessageBox.confirm(
        'You have unsaved changes. Discard and leave?',
        'Unsaved Changes',
        {
          confirmButtonText: 'Discard',
          cancelButtonText: 'Stay',
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
  cfg.fetch().then(() => {
    if (cfg.config.value) {
      Object.assign(config, cfg.config.value)
    }
  })
})

// --- Error handling ---
watch(() => cfg.error.value, (err) => {
  if (err) {
    ElNotification({
      title: 'Error',
      message: err,
      type: 'error',
      duration: 5000,
    })
  }
})

onUnmounted(() => {
  window.removeEventListener('beforeunload', handleBeforeUnload)
  window.removeEventListener('resize', handleResize)
  Object.values(revealTimers).forEach(clearInterval)
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
