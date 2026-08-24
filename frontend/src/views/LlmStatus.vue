<script setup lang="ts">
import { ref, computed, watch, onMounted, onUnmounted } from 'vue'
import { ElNotification } from 'element-plus'
import { useI18n } from 'vue-i18n'
import { RefreshRight, Cpu, CircleCheck, Warning, CircleClose, Remove, Edit, Check, Close } from '@element-plus/icons-vue'
import { useLlmStatus } from '../composables/useLlmStatus'
import { useConfig } from '../composables/useConfig'
import { useConfigForm } from '../composables/useConfigForm'
import { useProviders } from '../composables/useProviders'
import { getSystemHealth } from '../services/health'
import ProviderCard from '../components/LlmStatus/ProviderCard.vue'
import LlmSettingsCard from '../components/Config/LlmSettingsCard.vue'
import ProvidersSection from '../components/Config/ProvidersSection.vue'
import type { LlmProvider } from '../types/llm'

/* ------------------------------------------------------------------ */
/*  Composable                                                        */
/* ------------------------------------------------------------------ */

const { t } = useI18n()
const llm = useLlmStatus()

const providers = llm.providers
const loading = llm.loading
const cardRefs = ref<InstanceType<typeof ProviderCard>[]>([])

const testingMap = computed<Record<string, boolean>>(() => {
  if (!llm.testingId.value) return {}
  return { [llm.testingId.value]: true }
})

const healthyCount = computed(() => llm.healthyCount.value)
const degradedCount = computed(() => llm.degradedCount.value)
const errorCount = computed(() => llm.errorCount.value)
const offlineCount = computed(() => llm.offlineCount.value)

const avgLatency = computed(() => {
  const active = providers.value.filter(p => p.configured && p.status !== 'offline' && p.latencyMs > 0)
  if (!active.length) return 0
  return Math.round(active.reduce((sum, p) => sum + p.latencyMs, 0) / active.length)
})

const totalRequests = computed(() =>
  providers.value.reduce((sum, p) => sum + p.requestCount, 0)
)

/* ------------------------------------------------------------------ */
/*  LLM Configuration (edit lifecycle local to this section)          */
/* ------------------------------------------------------------------ */

const cfg = useConfig()

const {
  config,
  isEditing: cfgEditing,
  formRef: cfgFormRef,
  configDirty,
  rules: cfgRules,
  availableModels,
  modelFetchLoading,
  modelFetchError,
  enterEditMode,
  restoreSnapshot,
  commitSnapshot,
  loadConfig,
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
} = useProviders(cfgEditing, configDirty)

const configLoading = cfg.loading
const cfgSaving = cfg.saving
const cfgTesting = cfg.testing
const cfgTestResult = cfg.testResult
/** True when the server reports no usable LLM via /system/health. */
const llmNotConfigured = ref(false)
/** Card ref for the save flash animation (LlmSettingsCard only —
 *  ProvidersSection is multi-root, so its $el is not the card element). */
const llmSettingsCardRef = ref<HTMLElement>()

/** Dirty across the LLM form and the additional-providers list. */
const cfgDirtyAll = computed(() => configDirty.value || providersDirty.value)

// Responsive label layout, same breakpoint convention as Configuration.vue
const windowWidth = ref(window.innerWidth)
const labelPosition = computed(() => (windowWidth.value >= 1024 ? 'left' : 'top'))

function handleResize() {
  windowWidth.value = window.innerWidth
}

/** Refresh the not-configured banner. Fail-open: a health-check error keeps
 *  the current banner state. */
function checkLlmConfigured() {
  getSystemHealth()
    .then((health) => {
      llmNotConfigured.value = health.llmConfigured === false
    })
    .catch(() => {})
}

function cancelConfigEdit() {
  restoreSnapshot()
  // Discard unsaved provider edits as well, otherwise they would keep the
  // section dirty the next time edit mode is entered.
  resetProviders()
}

async function saveConfigChanges() {
  // Provider-only changes are independent of the LLM form: skip form
  // validation, which would otherwise block provider management when the
  // primary LLM config is incomplete.
  if (!configDirty.value && providersDirty.value) {
    await saveProvidersOnly()
    llm.fetch()
    checkLlmConfigured()
    return
  }
  if (!cfgFormRef.value) return
  const valid = await cfgFormRef.value.validate().catch(() => false)
  // Validation failures keep their inline errors but must not block saving:
  // the backend treats blank/masked secrets as "keep the stored value", so a
  // partially-filled form saves safely. Warn, then save with what is present.
  if (!valid) {
    ElNotification({
      title: t('config.validation.title'),
      message: t('config.validation.saveWithWarnings'),
      type: 'warning',
      duration: 4000,
    })
  }

  try {
    // Sparse PUT: send ONLY the `llm` section. The backend deep-merges the
    // payload over the stored config — omitted sections (gitlab/rules/…)
    // stay untouched, and masked (`***`)/blank API keys keep the stored
    // secrets. `config.llm` carries the legacy scalar fields plus the
    // `providers` array echoed by GET /config (masked keys), assembled
    // exactly like the Configuration page's save did.
    await cfg.save({ llm: JSON.parse(JSON.stringify(config.llm)) })
    await saveAdditionalProviders()
    commitSnapshot()

    ElNotification({
      title: t('common.success'),
      message: t('config.saved'),
      type: 'success',
      duration: 3000,
    })

    // Flash border animation on the saved card. The template ref on
    // <el-card> resolves to the component instance, so the root node must be
    // reached via `$el`.
    const el = (llmSettingsCardRef.value as unknown as { $el?: HTMLElement })?.$el
    if (el?.classList) {
      el.classList.add('flash-success')
      setTimeout(() => el.classList.remove('flash-success'), 600)
    }

    // Reflect the new config in the health cards and the banner.
    llm.fetch()
    checkLlmConfigured()
  } catch {
    ElNotification({
      title: t('common.error'),
      message: t('config.saveFailed'),
      type: 'error',
      duration: 5000,
    })
  }
}

/* ------------------------------------------------------------------ */
/*  Error Handling                                                    */
/* ------------------------------------------------------------------ */

watch(() => llm.error.value, (err) => {
  if (err) {
    ElNotification({
      title: t('common.error'),
      message: err,
      type: 'error',
      duration: 5000,
    })
  }
})

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

/* ------------------------------------------------------------------ */
/*  Actions                                                           */
/* ------------------------------------------------------------------ */

function fetchProviders() {
  llm.fetch()
}

function handleRefreshAll() {
  llm.fetch().then(() => {
    const healthy = healthyCount.value
    const issues = degradedCount.value + errorCount.value

    ElNotification({
      title: t('llm.refreshedTitle'),
      message: t('llm.refreshedMessage', { healthy, issues }),
      type: issues === 0 ? 'success' : 'warning',
      duration: issues === 0 ? 3000 : 5000,
    })
  })
}

async function handleTestSingle(provider: LlmProvider) {
  try {
    const result = await llm.test(provider.id)
    const card = cardRefs.value.find(c => c.providerId === provider.id)
    if (card) {
      card.showTestResult(result)
    }
  } catch {
    // Error already handled by composable
  }
}

/* ------------------------------------------------------------------ */
/*  Lifecycle                                                         */
/* ------------------------------------------------------------------ */

onMounted(() => {
  fetchProviders()
  loadConfig()
  loadProviders()
  checkLlmConfigured()
  window.addEventListener('resize', handleResize)
})

onUnmounted(() => {
  window.removeEventListener('resize', handleResize)
})
</script>

<template>
  <div class="llm-page">
    <!-- Page Header -->
    <div class="page-header">
      <div class="header-text">
        <h2 class="page-title">{{ $t('llm.title') }}</h2>
        <p class="page-subtitle">{{ $t('llm.subtitle') }}</p>
      </div>
      <el-button
        type="primary"
        :icon="RefreshRight"
        :loading="loading"
        @click="handleRefreshAll"
      >
        {{ $t('llm.refreshAll') }}
      </el-button>
    </div>

    <!-- Loading Skeleton -->
    <div v-if="loading && providers.length === 0" class="skeleton-grid">
      <el-skeleton
        v-for="i in 6"
        :key="i"
        animated
        :rows="4"
        class="skeleton-card"
      />
    </div>

    <!-- Empty State -->
    <el-empty
      v-else-if="providers.length === 0"
      :description="$t('llm.noProviders')"
    >
      <el-button type="primary" @click="fetchProviders">{{ $t('llm.reload') }}</el-button>
    </el-empty>

    <!-- Content -->
    <template v-else>
      <!-- Summary Stats -->
      <div class="stats-row">
        <el-card shadow="never" class="stat-card">
          <div class="stat-content">
            <el-icon class="stat-icon" :size="24"><Cpu /></el-icon>
            <div class="stat-body">
              <div class="stat-value">{{ providers.length }}</div>
              <div class="stat-label">{{ $t('llm.stats.providers') }}</div>
            </div>
          </div>
        </el-card>
        <el-card shadow="never" class="stat-card">
          <div class="stat-content">
            <el-icon class="stat-icon" :size="24" color="var(--success)"><CircleCheck /></el-icon>
            <div class="stat-body">
              <div class="stat-value" style="color: var(--success)">{{ healthyCount }}</div>
              <div class="stat-label">{{ $t('llm.status.healthy') }}</div>
            </div>
          </div>
        </el-card>
        <el-card shadow="never" class="stat-card">
          <div class="stat-content">
            <el-icon class="stat-icon" :size="24" color="var(--warning)"><Warning /></el-icon>
            <div class="stat-body">
              <div class="stat-value" style="color: var(--warning)">{{ degradedCount }}</div>
              <div class="stat-label">{{ $t('llm.status.degraded') }}</div>
            </div>
          </div>
        </el-card>
        <el-card shadow="never" class="stat-card">
          <div class="stat-content">
            <el-icon class="stat-icon" :size="24" color="var(--error)"><CircleClose /></el-icon>
            <div class="stat-body">
              <div class="stat-value" style="color: var(--error)">{{ errorCount }}</div>
              <div class="stat-label">{{ $t('llm.status.error') }}</div>
            </div>
          </div>
        </el-card>
        <el-card shadow="never" class="stat-card">
          <div class="stat-content">
            <el-icon class="stat-icon" :size="24" color="var(--offline)"><Remove /></el-icon>
            <div class="stat-body">
              <div class="stat-value" style="color: var(--offline)">{{ offlineCount }}</div>
              <div class="stat-label">{{ $t('llm.status.offline') }}</div>
            </div>
          </div>
        </el-card>
        <el-card shadow="never" class="stat-card">
          <div class="stat-content">
            <el-icon class="stat-icon" :size="24"><RefreshRight /></el-icon>
            <div class="stat-body">
              <div class="stat-value">{{ avgLatency }} ms</div>
              <div class="stat-label">{{ $t('llm.stats.avgLatency') }}</div>
            </div>
          </div>
        </el-card>
        <el-card shadow="never" class="stat-card">
          <div class="stat-content">
            <el-icon class="stat-icon" :size="24"><Cpu /></el-icon>
            <div class="stat-body">
              <div class="stat-value">{{ new Intl.NumberFormat('en-US').format(totalRequests) }}</div>
              <div class="stat-label">{{ $t('llm.stats.totalRequests') }}</div>
            </div>
          </div>
        </el-card>
      </div>

      <!-- Provider Grid -->
      <div class="provider-grid">
        <ProviderCard
          v-for="(provider, idx) in providers"
          :key="provider.id"
          ref="cardRefs"
          :provider="provider"
          :index="idx"
          :testing="testingMap[provider.id]"
          :loading="loading"
          @test="handleTestSingle"
        />
      </div>
    </template>

    <!-- LLM Configuration Section (own edit lifecycle, independent of the
         status section above, which stays fully functional while editing) -->
    <section id="llm-config-section" class="llm-config-section">
      <div class="config-section-header">
        <h3 class="section-title">{{ $t('llm.configSection') }}</h3>
        <div class="header-actions">
          <template v-if="!cfgEditing">
            <el-button type="primary" :disabled="configLoading" @click="enterEditMode">
              <el-icon><Edit /></el-icon>
              <span>{{ $t('config.editBtn') }}</span>
            </el-button>
          </template>
          <template v-else>
            <!-- Tooltip explains why Save is disabled; the wrapper span is needed
                 because a disabled button swallows pointer events. -->
            <el-tooltip :content="$t('config.noChangesToSave')" :disabled="cfgDirtyAll" placement="top">
              <span class="save-button-wrapper">
                <el-badge :is-dot="cfgDirtyAll" type="danger">
                  <el-button type="primary" :loading="cfgSaving" :disabled="!cfgDirtyAll" @click="saveConfigChanges">
                    <el-icon><Check /></el-icon>
                    <span>{{ $t('common.saveChanges') }}</span>
                  </el-button>
                </el-badge>
              </span>
            </el-tooltip>
            <el-button @click="cancelConfigEdit">
              <el-icon><Close /></el-icon>
              <span>{{ $t('common.cancel') }}</span>
            </el-button>
          </template>
        </div>
      </div>

      <!-- LLM-not-configured banner: reviews cannot run without a usable LLM -->
      <el-alert
        v-if="llmNotConfigured"
        type="warning"
        :closable="false"
        :title="$t('config.llmNotConfiguredBanner')"
        class="llm-banner"
      />

      <el-form
        ref="cfgFormRef"
        :model="config"
        :rules="cfgRules"
        :disabled="!cfgEditing"
        :label-position="labelPosition"
        label-width="auto"
        class="config-form"
        @submit.prevent
      >
        <!-- LLM Card -->
        <LlmSettingsCard
          ref="llmSettingsCardRef"
          :config="config.llm"
          :is-editing="cfgEditing"
          :models="availableModels"
          :model-fetch-loading="modelFetchLoading"
          :model-fetch-error="modelFetchError"
          :testing="cfgTesting"
          :test-result="cfgTestResult"
          @test="testConnection"
        />

        <!-- Additional LLM Providers Card -->
        <ProvidersSection
          :providers="additionalProviders"
          :is-editing="cfgEditing"
          :loading="providersLoading"
          @toggle="toggleProvider"
          @remove="confirmDeleteProvider"
          @add="addProvider"
        />
      </el-form>

      <!-- Mobile Sticky Actions -->
      <div v-if="cfgEditing" class="mobile-actions">
        <el-tooltip :content="$t('config.noChangesToSave')" :disabled="cfgDirtyAll" placement="top">
          <span class="save-button-wrapper">
            <el-badge :is-dot="cfgDirtyAll" type="danger" class="mobile-badge">
              <el-button type="primary" :loading="cfgSaving" :disabled="!cfgDirtyAll" @click="saveConfigChanges">
                {{ $t('common.saveChanges') }}
              </el-button>
            </el-badge>
          </span>
        </el-tooltip>
        <el-button @click="cancelConfigEdit">{{ $t('common.cancel') }}</el-button>
      </div>
    </section>
  </div>
</template>

<style scoped>
.llm-page {
  max-width: 1400px;
  margin: 0 auto;
}

.page-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  margin-bottom: 24px;
  flex-wrap: wrap;
}

.header-text {
  flex: 1;
}

.page-title {
  font-size: 24px;
  font-weight: 600;
  color: var(--text-primary);
  margin-bottom: 4px;
  letter-spacing: -0.02em;
  line-height: 1.3;
}

.page-subtitle {
  font-size: 14px;
  color: var(--text-secondary);
  margin: 0;
}

/* Stats Row */
.stats-row {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(160px, 1fr));
  gap: 12px;
  margin-bottom: 24px;
}

.stat-card :deep(.el-card__body) {
  padding: 16px;
}

.stat-content {
  display: flex;
  align-items: center;
  gap: 12px;
}

.stat-icon {
  color: var(--text-secondary);
  flex-shrink: 0;
}

.stat-body {
  flex: 1;
  min-width: 0;
}

.stat-value {
  font-family: var(--font-mono);
  font-size: 20px;
  font-weight: 600;
  color: var(--text-primary);
  line-height: 1.2;
}

.stat-label {
  font-size: 12px;
  color: var(--text-secondary);
  margin-top: 2px;
}

/* Skeleton */
.skeleton-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(320px, 1fr));
  gap: 16px;
}

.skeleton-card {
  padding: 20px;
  background: var(--bg-card);
  border-radius: var(--radius-md);
  border: 1px solid var(--border-color);
}

/* Provider Grid */
.provider-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(320px, 1fr));
  gap: 16px;
}

/* ------------------------------------------------------------------ */
/*  LLM Configuration section (visual language mirrors Configuration)  */
/* ------------------------------------------------------------------ */

.llm-config-section {
  margin-top: 32px;
}

.config-section-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  margin-bottom: 16px;
  flex-wrap: wrap;
}

.section-title {
  font-size: 18px;
  font-weight: 600;
  color: var(--text-primary);
  margin: 0;
  letter-spacing: -0.02em;
  line-height: 1.3;
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
  transition: all 0.15s ease;
}

/* Wrapper lets the tooltip hover target survive the disabled save button */
.save-button-wrapper {
  display: inline-flex;
}

/* LLM-not-configured banner sits below the section header, above the cards */
.llm-banner {
  margin-bottom: 20px;
}

.config-form {
  display: flex;
  flex-direction: column;
  gap: 20px;
}

/* Card Design System — the moved config cards rely on the parent view for
   their themed shell (scoped styles reach the child component root). */
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

.config-card :deep(.el-card__body) {
  max-height: none;
  overflow: visible;
}

/* Flash animation on successful save */
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
@media (max-width: 768px) {
  .page-header {
    flex-direction: column;
    align-items: stretch;
  }

  .stats-row {
    grid-template-columns: repeat(2, 1fr);
  }

  .provider-grid {
    grid-template-columns: 1fr;
  }

  .skeleton-grid {
    grid-template-columns: 1fr;
  }

  .llm-config-section .header-actions {
    display: none;
  }

  .llm-config-section .mobile-actions {
    display: flex;
  }

  .config-section-header {
    flex-direction: column;
    align-items: flex-start;
  }
}

@media (min-width: 769px) and (max-width: 1024px) {
  .provider-grid {
    grid-template-columns: repeat(2, 1fr);
  }

  .skeleton-grid {
    grid-template-columns: repeat(2, 1fr);
  }
}

@media (min-width: 1025px) and (max-width: 1279px) {
  .provider-grid {
    grid-template-columns: repeat(3, 1fr);
  }

  .skeleton-grid {
    grid-template-columns: repeat(3, 1fr);
  }
}

@media (min-width: 1280px) {
  .provider-grid {
    grid-template-columns: repeat(4, 1fr);
  }

  .skeleton-grid {
    grid-template-columns: repeat(4, 1fr);
  }
}
</style>
