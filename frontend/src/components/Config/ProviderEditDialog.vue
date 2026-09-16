<script setup lang="ts">
import { computed, reactive, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { Connection, Plus } from '@element-plus/icons-vue'
import { ElMessageBox, type FormInstance, type FormRules } from 'element-plus'
import { PROVIDER_TYPES } from '../../types/llm'
import type { TestResult } from '../../types/llm'
import type { CatalogProvider } from '../../types/catalog'
import { useCatalog } from '../../composables/useCatalog'
import { fetchModels, testConnection } from '../../services/config'
import {
  createEmptyProviderCard,
  type ProviderCardState,
} from '../../composables/llmPayload'
import {
  DUPLICATE_UNCHANGED_KEY,
  duplicateUnchanged,
  initialDialogForm,
  type ProviderDialogMode,
} from './providerCardState'

const props = defineProps<{
  /** Dialog visibility (v-model:visible). */
  visible: boolean
  /** Add a new provider, edit an existing card, or edit a copy of one before
   *  saving it as a new card (RENG-83). */
  mode: ProviderDialogMode
  /** The card the form starts from: the one an edit replaces, or the one a
   *  duplicate copies. Ignored by `add`, which always opens empty. */
  initial?: ProviderCardState | null
  /**
   * RENG-75: the provider NAME is a display label, not an identity — two
   * cards may share one (two accounts, or one account × two models) — so the
   * add form no longer rejects a name a sibling already uses. The real
   * identity is the `(provider, base, model, key)` fingerprint, and the only
   * part of it the client can see is the triple, which is legitimate to
   * repeat: a duplicate starts as a sibling with the same credential.
   */
  saving: boolean
}>()

const emit = defineEmits<{
  (e: 'update:visible', value: boolean): void
  /** Validated form submitted; the parent persists it and closes on success. */
  (e: 'save', form: ProviderCardState): void
}>()

const { t } = useI18n()

const dialogVisible = computed({
  get: () => props.visible,
  set: (v: boolean) => emit('update:visible', v),
})

const formRef = ref<FormInstance>()
const form = reactive<ProviderCardState>(createEmptyProviderCard())

/** RENG-83: the values a `duplicate` dialog opened with, so a save that
 *  changed nothing can be pointed out before it is written. `null` in the other
 *  two modes, which have nothing to compare against. */
const duplicateSnapshot = ref<ProviderCardState | null>(null)

// --- Provider type select (models.dev catalog with preset fallback) ---
const { catalogProviders, catalogFailed, loadCatalogProviders } = useCatalog()
const selectedCatalogProvider = ref<CatalogProvider | null>(null)

const catalogAvailable = computed(() => catalogProviders.value.length > 0)
const presetProviderTypes = computed(() => PROVIDER_TYPES.filter((pt) => pt.value !== 'custom'))

/** `edit` and `duplicate` both pre-fill the type select: the card they start
 *  from may name a provider that is absent from both the catalog and the
 *  preset list (e.g. a removed catalog entry like "xiaomi-mimo"). The select
 *  would then render the raw id as bare text, so inject it as a temporary
 *  option (label = id) to display a proper selected value. `add` always starts
 *  from `custom`, which is an option. */
const currentProviderMissing = computed(() => {
  if (props.mode === 'add' || !form.provider || form.provider === 'custom') return false
  if (catalogAvailable.value) {
    return !catalogProviders.value.some((p) => p.id === form.provider)
  }
  return !presetProviderTypes.value.some((pt) => pt.value === form.provider)
})

/** A blank key means "keep the saved key" in every mode that pre-fills the
 *  form — `edit` and `duplicate` alike (RENG-83): the browser only ever sees
 *  the mask, and the server resolves the blank back to a stored key by the
 *  card's triple. Only a genuinely new card needs a key typed in. */
const apiKeyPlaceholder = computed(() => {
  if (props.mode !== 'add') return t('config.providerCards.keepKeyPlaceholder')
  return selectedCatalogProvider.value?.env?.[0] || t('config.providers.apiKeyPlaceholder')
})

// --- Model list (POST /config/models by api_base; masked-key fallback is
//     server-side, so edit mode fetches with the blank "keep" key) ---
const modelOptions = ref<string[]>([])
const modelsLoading = ref(false)
const modelsError = ref<string | null>(null)
let modelFetchTimer: number | null = null
// Sequence guard so a slow response never overwrites a newer fetch.
let modelRequestSeq = 0

// --- Connection test ---
const testing = ref(false)
const testResult = ref<TestResult | null>(null)

// --- Advanced group ---
const advancedActive = ref<string[]>([])

const rules = computed<FormRules>(() => ({
  provider: [
    { required: true, message: t('config.providerCards.providerRequired'), trigger: 'change' },
  ],
  apiKey: [
    {
      // Edit mode keeps the saved key when the field is left blank; adding a
      // provider always requires one (the backend drops keyless entries).
      required: props.mode === 'add',
      message: t('config.providerCards.apiKeyRequired'),
      trigger: 'blur',
    },
  ],
  apiBaseUrl: [
    {
      validator: (_rule: unknown, value: string, callback: (e?: Error) => void) => {
        if (!value || !value.trim()) return callback()
        try {
          new URL(value)
          callback()
        } catch {
          callback(new Error(t('config.validation.invalidUrl')))
        }
      },
      trigger: 'blur',
    },
  ],
  defaultModel: [
    { required: true, message: t('config.validation.modelRequired'), trigger: 'change' },
  ],
}))

/** True when a model fetch is worthwhile: a parseable base URL, and either a
 *  typed key or a pre-filled "keep" blank (which the server resolves to the
 *  stored key for the same api_base). `duplicate` qualifies like `edit`: a
 *  copied card has its base URL pre-filled and its key field blank, and the
 *  dropdown is exactly what the user needs to pick the one field they are
 *  changing. */
function canFetchModels(): boolean {
  const base = form.apiBaseUrl.trim()
  if (!base) return false
  try {
    new URL(base)
  } catch {
    return false
  }
  return !!form.apiKey.trim() || props.mode !== 'add'
}

async function loadModelOptions() {
  const seq = ++modelRequestSeq
  if (!canFetchModels()) {
    modelOptions.value = []
    modelsError.value = null
    return
  }
  modelsLoading.value = true
  modelsError.value = null
  try {
    const resp = await fetchModels(form.apiBaseUrl.trim(), form.apiKey.trim())
    if (seq !== modelRequestSeq) return
    if (resp.error) {
      modelOptions.value = []
      modelsError.value = resp.error
    } else {
      modelOptions.value = resp.models ?? []
      // Convenience prefill only — never clobber a value the user picked.
      if (!form.defaultModel && modelOptions.value.length > 0) {
        form.defaultModel = modelOptions.value[0]
      }
    }
  } catch (e) {
    if (seq !== modelRequestSeq) return
    modelOptions.value = []
    modelsError.value = e instanceof Error ? e.message : String(e)
  } finally {
    if (seq === modelRequestSeq) modelsLoading.value = false
  }
}

watch(
  () => [form.apiBaseUrl, form.apiKey],
  () => {
    if (!props.visible) return
    if (modelFetchTimer) clearTimeout(modelFetchTimer)
    modelFetchTimer = window.setTimeout(() => {
      void loadModelOptions()
    }, 500)
  },
)

/**
 * Initialize the form every time the dialog opens.
 *
 * RENG-83: the values come from {@link initialDialogForm}, which is the piece
 * that used to be missing here — `add` mode discarded the card a duplicate had
 * passed in, so 「复制卡片」 opened an empty form. The source card is also kept
 * as the snapshot the save compares against, so an untouched copy can be
 * pointed out before it is written.
 */
watch(
  () => props.visible,
  async (open) => {
    if (!open) return
    if (modelFetchTimer) {
      clearTimeout(modelFetchTimer)
      modelFetchTimer = null
    }
    modelRequestSeq++
    const opened = initialDialogForm(props.mode, props.initial)
    Object.assign(form, opened)
    // Snapshot of what the duplicate dialog STARTED with: a save that still
    // equals it added nothing the source card did not already say.
    duplicateSnapshot.value = props.mode === 'duplicate' ? { ...opened } : null
    selectedCatalogProvider.value =
      catalogProviders.value.find((p) => p.id === form.provider) ?? null
    modelOptions.value = []
    modelsError.value = null
    testResult.value = null
    advancedActive.value = []
    if (props.mode !== 'edit') {
      // Force a retry when the previous attempt failed — a 503 may be
      // transient. Edit mode hides the type select, so skip the fetch.
      void loadCatalogProviders(catalogFailed.value)
    }
    // Preload the model list for the prefilled base URL.
    void loadModelOptions()
    // Drop stale validation messages from a previous dialog session.
    formRef.value?.clearValidate()
  },
)

/** Provider select change (add mode only): prefill the base URL from the
 *  catalog entry; preset/custom entries leave the field as-is. */
function onProviderChange(value: string) {
  const entry = catalogProviders.value.find((p) => p.id === value) ?? null
  selectedCatalogProvider.value = entry
  if (entry) form.apiBaseUrl = entry.api_base
}

async function runTest() {
  testing.value = true
  testResult.value = null
  try {
    testResult.value = await testConnection({
      provider: form.provider,
      model: form.defaultModel.trim(),
      apiKey: form.apiKey.trim(),
      apiBase: form.apiBaseUrl.trim(),
    })
  } catch (e) {
    testResult.value = {
      success: false,
      error: e instanceof Error ? e.message : String(e),
      timestamp: new Date().toISOString(),
    }
  } finally {
    testing.value = false
  }
}

async function confirm() {
  if (!formRef.value) return
  const valid = await formRef.value.validate().catch(() => false)
  if (!valid) return
  const submitted: ProviderCardState = {
    ...form,
    provider: form.provider.trim(),
    apiKey: form.apiKey.trim(),
    apiBaseUrl: form.apiBaseUrl.trim(),
    defaultModel: form.defaultModel.trim(),
  }
  // RENG-83: a copy saved without a single change adds a second card saying
  // exactly what the first one says. Warn — do not refuse: the client cannot
  // see the API key, so two cards this view calls identical may differ in the
  // one field it never reads (RENG-75 allows a same-triple pair), and a hard
  // block would take that decision away from the user.
  if (
    props.mode === 'duplicate' &&
    duplicateSnapshot.value &&
    duplicateUnchanged(duplicateSnapshot.value, submitted)
  ) {
    try {
      await ElMessageBox.confirm(t(DUPLICATE_UNCHANGED_KEY), t('config.providerCards.duplicate'), {
        confirmButtonText: t('common.save'),
        cancelButtonText: t('common.cancel'),
        type: 'warning',
      })
    } catch {
      // Dismissed: the user stays in the dialog to change something first.
      return
    }
  }
  emit('save', submitted)
}
</script>

<template>
  <el-dialog
    v-model="dialogVisible"
    :title="mode === 'edit' ? $t('config.providerCards.editTitle') : $t('config.providerCards.addTitle')"
    width="var(--modal-w-lg)"
    append-to-body
    :close-on-click-modal="false"
  >
    <el-form ref="formRef" :model="form" :rules="rules" label-position="top" @submit.prevent>
      <el-alert
        v-if="mode !== 'edit' && catalogFailed"
        type="warning"
        :closable="false"
        :title="$t('config.providers.catalogUnavailable')"
        class="catalog-fallback-alert"
      />
      <el-row :gutter="20">
        <el-col :span="12">
          <el-form-item :label="$t('config.providers.providerType')" prop="provider">
            <el-select
              v-model="form.provider"
              filterable
              :disabled="mode === 'edit'"
              style="width: 100%"
              @change="onProviderChange"
            >
              <el-option
                v-if="currentProviderMissing"
                :label="form.provider"
                :value="form.provider"
              />
              <el-option :label="$t('config.providers.customProvider')" value="custom" />
              <template v-if="catalogAvailable">
                <el-option v-for="p in catalogProviders" :key="p.id" :label="p.name" :value="p.id">
                  <div class="provider-option">
                    <span>{{ p.name }}</span>
                    <span class="provider-option-meta">
                      {{ p.id }} ·
                      {{ $t('config.providers.modelsCount', { count: p.model_count }) }}
                    </span>
                  </div>
                </el-option>
              </template>
              <template v-else>
                <el-option
                  v-for="pt in presetProviderTypes"
                  :key="pt.value"
                  :label="pt.label"
                  :value="pt.value"
                />
              </template>
            </el-select>
            <div v-if="mode !== 'edit' && selectedCatalogProvider?.doc" class="form-item-help">
              <el-link
                :href="selectedCatalogProvider.doc"
                target="_blank"
                rel="noopener noreferrer"
                type="info"
              >
                {{ $t('config.providers.viewDocs') }}
              </el-link>
            </div>
          </el-form-item>
        </el-col>
        <el-col :span="12">
          <el-form-item :label="$t('config.providers.apiKey')" prop="apiKey">
            <el-input v-model="form.apiKey" show-password :placeholder="apiKeyPlaceholder" />
          </el-form-item>
        </el-col>
        <el-col :span="24">
          <el-form-item :label="$t('config.providers.apiBaseUrl')" prop="apiBaseUrl">
            <el-input
              v-model="form.apiBaseUrl"
              :placeholder="$t('config.providers.apiBasePlaceholder')"
            />
          </el-form-item>
        </el-col>
        <el-col :span="24">
          <el-form-item :label="$t('config.providers.defaultModel')" prop="defaultModel">
            <el-select
              v-model="form.defaultModel"
              filterable
              allow-create
              default-first-option
              clearable
              :loading="modelsLoading"
              :placeholder="$t('config.providers.modelPlaceholder')"
              style="width: 100%"
            >
              <el-option v-for="m in modelOptions" :key="m" :label="m" :value="m" />
            </el-select>
            <div v-if="modelsError" class="form-item-help error-text">
              {{ $t('config.providers.modelsFallback') }} ({{ modelsError }})
            </div>
          </el-form-item>
        </el-col>
      </el-row>

      <el-collapse v-model="advancedActive" class="advanced-collapse">
        <el-collapse-item :title="$t('config.providerCards.advanced')" name="advanced">
          <el-row :gutter="20">
            <el-col :xs="12" :sm="12">
              <el-form-item :label="$t('config.providers.maxTokens')">
                <el-input-number
                  v-model="form.maxTokens"
                  :min="128"
                  :max="8192"
                  :step="128"
                  style="width: 100%"
                />
              </el-form-item>
            </el-col>
            <el-col :xs="12" :sm="12">
              <el-form-item :label="$t('config.providers.retryAttempts')">
                <el-input-number v-model="form.retryAttempts" :min="0" :max="5" style="width: 100%" />
              </el-form-item>
            </el-col>
            <el-col :xs="12" :sm="12">
              <el-form-item :label="$t('config.providers.timeoutShort')">
                <el-input-number
                  v-model="form.timeoutSeconds"
                  :min="5"
                  :max="300"
                  :step="5"
                  style="width: 100%"
                />
              </el-form-item>
            </el-col>
            <el-col :xs="12" :sm="12">
              <el-form-item :label="$t('config.providers.temperature')">
                <div class="slider-with-value">
                  <el-slider v-model="form.temperature" :min="0" :max="2" :step="0.1" />
                  <span class="slider-value">{{ form.temperature.toFixed(1) }}</span>
                </div>
              </el-form-item>
            </el-col>
            <el-col :span="24">
              <!-- RENG-77: a reasoning model otherwise spends its whole output
                   budget on hidden reasoning and answers with nothing. -->
              <el-form-item :label="$t('config.providers.disableThinking')">
                <div class="switch-row">
                  <el-switch v-model="form.disableThinking" />
                  <span class="form-item-help switch-row__hint">
                    {{ $t('config.providers.disableThinkingHint') }}
                  </span>
                </div>
              </el-form-item>
            </el-col>
          </el-row>
        </el-collapse-item>
      </el-collapse>
    </el-form>

    <template #footer>
      <div class="dialog-footer">
        <div class="footer-test">
          <el-button :loading="testing" @click="runTest">
            <el-icon><Connection /></el-icon>
            <span>{{ $t('common.testConnection') }}</span>
          </el-button>
          <el-tag v-if="testResult" :type="testResult.success ? 'success' : 'danger'" effect="dark">
            {{
              testResult.success
                ? $t('config.llm.connected', { n: testResult.latencyMs })
                : $t('config.llm.testFailed', { error: testResult.error })
            }}
          </el-tag>
        </div>
        <div class="footer-actions">
          <el-button @click="dialogVisible = false">{{ $t('common.cancel') }}</el-button>
          <el-button type="primary" :loading="saving" @click="confirm">
            <el-icon v-if="mode !== 'edit'"><Plus /></el-icon>
            <span>{{ mode !== 'edit' ? $t('config.providerCards.add') : $t('common.save') }}</span>
          </el-button>
        </div>
      </div>
    </template>
  </el-dialog>
</template>

<style scoped>
.catalog-fallback-alert {
  margin-bottom: var(--space-4);
}

.form-item-help {
  font-size: 12px;
  color: var(--text-secondary);
  margin-top: 6px;
  line-height: 1.4;
}

.form-item-help.error-text {
  color: var(--accent-error);
}

.provider-option {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--space-3);
}

.provider-option-meta {
  font-size: 12px;
  color: var(--text-secondary);
}

/* The dialog body is --bg-card, but el-collapse's default header/wrap paint
   --el-fill-color-blank (white in light, --bg-surface in dark), showing up as
   a mismatched gray block. Make the collapse fully transparent/borderless and
   keep only the subtle top divider as a visual separator. */
.advanced-collapse {
  margin-top: var(--space-1);
  border-top: 1px solid var(--border-color);
  border-bottom: none;
}

.advanced-collapse :deep(.el-collapse-item__header),
.advanced-collapse :deep(.el-collapse-item__wrap) {
  background-color: transparent;
  border-bottom: none;
}

.slider-with-value {
  display: flex;
  align-items: center;
  gap: var(--space-3);
  width: 100%;
}

/* RENG-77: the switch and the sentence explaining it on one row — the label
   above them names the setting, the hint sits beside the control. */
.switch-row {
  display: flex;
  align-items: center;
  gap: var(--space-3);
  width: 100%;
}

.switch-row__hint {
  flex: 1;
  min-width: 0;
  margin-top: 0;
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

.dialog-footer {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--space-3);
  flex-wrap: wrap;
}

.footer-test {
  display: flex;
  align-items: center;
  gap: var(--space-3);
}

.footer-actions {
  display: flex;
  align-items: center;
  gap: 10px;
}
</style>
