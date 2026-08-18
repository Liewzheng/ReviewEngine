<template>
  <!-- Additional LLM Providers Card -->
  <el-card class="config-card additional-providers-card">
    <template #header>
      <div class="card-header">
        <el-icon><Cpu /></el-icon>
        <span>{{ $t('config.providers.title') }}</span>
        <div v-if="isEditing" class="header-action">
          <el-button size="small" type="primary" @click="openAddProviderDialog">
            <el-icon><Plus /></el-icon>
            {{ $t('config.providers.addBtn') }}
          </el-button>
        </div>
      </div>
    </template>
    <div class="card-body">
      <el-skeleton v-if="loading" :rows="2" animated />
      <el-empty
        v-else-if="providers.length === 0"
        :description="$t('config.providers.empty')"
        :image-size="80"
      />
      <div v-else class="providers-list">
        <div
          v-for="(provider, index) in providers"
          :key="provider._key"
          class="provider-item"
          :class="{ 'is-expanded': provider._expanded }"
        >
          <div class="provider-item-header" @click="emit('toggle', index)">
            <div class="provider-item-info">
              <el-tag size="small">{{ provider.provider }}</el-tag>
              <span class="provider-item-model">{{ provider.defaultModel || '—' }}</span>
              <span class="provider-item-base">{{ provider.apiBaseUrl }}</span>
            </div>
            <div class="provider-item-actions" @click.stop>
              <template v-if="isEditing">
                <el-button size="small" text @click.stop="emit('toggle', index)">
                  {{ provider._expanded ? $t('config.providers.collapse') : $t('common.edit') }}
                </el-button>
                <el-button size="small" text type="danger" @click.stop="emit('remove', index)">
                  <el-icon><Delete /></el-icon>
                </el-button>
              </template>
              <el-icon :class="{ 'rotated-open': provider._expanded }">
                <ArrowDown />
              </el-icon>
            </div>
          </div>
          <template v-if="provider._expanded">
            <div v-if="isEditing" class="provider-item-body">
              <el-form :model="provider" label-position="top" size="small">
                <el-row :gutter="16">
                  <el-col :xs="24" :sm="12">
                    <el-form-item :label="$t('config.providers.providerType')">
                      <el-select v-model="provider.provider" style="width: 100%">
                        <el-option
                          v-for="pt in PROVIDER_TYPES"
                          :key="pt.value"
                          :label="pt.label"
                          :value="pt.value"
                        />
                      </el-select>
                    </el-form-item>
                  </el-col>
                  <el-col :xs="24" :sm="12">
                    <el-form-item :label="$t('config.providers.defaultModel')">
                      <el-input
                        v-model="provider.defaultModel"
                        :placeholder="$t('config.providers.modelPlaceholder')"
                      />
                    </el-form-item>
                  </el-col>
                  <el-col :span="24">
                    <el-form-item :label="$t('config.providers.apiBaseUrl')">
                      <el-input
                        v-model="provider.apiBaseUrl"
                        :placeholder="$t('config.providers.apiBasePlaceholder')"
                      />
                    </el-form-item>
                  </el-col>
                  <el-col :span="24">
                    <el-form-item :label="$t('config.providers.apiKey')">
                      <el-input
                        v-model="provider.apiKey"
                        show-password
                        :placeholder="$t('config.providers.apiKeyPlaceholder')"
                      />
                    </el-form-item>
                  </el-col>
                  <el-col :xs="12" :sm="6">
                    <el-form-item :label="$t('config.providers.maxTokens')">
                      <el-input-number
                        v-model="provider.maxTokens"
                        :min="128"
                        :max="8192"
                        :step="128"
                        style="width: 100%"
                      />
                    </el-form-item>
                  </el-col>
                  <el-col :xs="12" :sm="6">
                    <el-form-item :label="$t('config.providers.temperature')">
                      <el-slider v-model="provider.temperature" :min="0" :max="2" :step="0.1" />
                    </el-form-item>
                  </el-col>
                  <el-col :xs="12" :sm="6">
                    <el-form-item :label="$t('config.providers.timeoutShort')">
                      <el-input-number
                        v-model="provider.timeout"
                        :min="5"
                        :max="300"
                        :step="5"
                        style="width: 100%"
                      />
                    </el-form-item>
                  </el-col>
                  <el-col :xs="12" :sm="6">
                    <el-form-item :label="$t('config.providers.retry')">
                      <el-input-number v-model="provider.retry" :min="0" :max="5" style="width: 100%" />
                    </el-form-item>
                  </el-col>
                </el-row>
              </el-form>
            </div>
            <div v-else class="provider-item-body readonly-body">
              <el-descriptions :column="2" size="small" border>
                <el-descriptions-item :label="$t('config.providers.providerType')">
                  {{ provider.provider }}
                </el-descriptions-item>
                <el-descriptions-item :label="$t('config.providers.defaultModel')">
                  {{ provider.defaultModel || '—' }}
                </el-descriptions-item>
                <el-descriptions-item :label="$t('config.providers.apiBaseUrl')" :span="2">
                  {{ provider.apiBaseUrl }}
                </el-descriptions-item>
                <el-descriptions-item
                  v-if="provider.maxTokens != null"
                  :label="$t('config.providers.maxTokens')"
                >
                  {{ provider.maxTokens }}
                </el-descriptions-item>
                <el-descriptions-item
                  v-if="provider.temperature != null"
                  :label="$t('config.providers.temperature')"
                >
                  {{ formatTemperature(provider.temperature) }}
                </el-descriptions-item>
                <el-descriptions-item
                  v-if="provider.timeout != null"
                  :label="$t('config.providers.timeoutShort')"
                >
                  {{ provider.timeout }}s
                </el-descriptions-item>
                <el-descriptions-item
                  v-if="provider.retry != null"
                  :label="$t('config.providers.retry')"
                >
                  {{ provider.retry }}
                </el-descriptions-item>
              </el-descriptions>
            </div>
          </template>
          <div v-if="provider._isNew && isEditing" class="provider-item-badge">
            <el-tag size="small" type="warning">{{ $t('config.providers.notSaved') }}</el-tag>
          </div>
        </div>
      </div>
    </div>
  </el-card>

  <!-- Add Provider Dialog -->
  <el-dialog
    v-model="showAddProviderDialog"
    :title="$t('config.providers.addDialogTitle')"
    width="640px"
    append-to-body
  >
    <el-form ref="addProviderFormRef" :model="newProvider" label-position="top" size="default">
      <el-alert
        v-if="catalogFailed"
        type="warning"
        :closable="false"
        :title="$t('config.providers.catalogUnavailable')"
        class="catalog-fallback-alert"
      />
      <el-row :gutter="20">
        <el-col :span="12">
          <el-form-item :label="$t('config.providers.providerType')" prop="provider">
            <el-select
              v-model="newProvider.provider"
              filterable
              style="width: 100%"
              @change="onDialogProviderChange"
            >
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
            <div v-if="selectedCatalogProvider?.doc" class="form-item-help">
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
          <el-form-item :label="$t('config.providers.defaultModel')">
            <el-select
              v-if="modelSelectActive"
              v-model="newProvider.defaultModel"
              filterable
              clearable
              :loading="modelsLoading"
              :placeholder="$t('config.providers.modelPlaceholder')"
              style="width: 100%"
            >
              <el-option v-for="m in catalogModels" :key="m.id" :label="m.name" :value="m.id">
                <div class="provider-option">
                  <span>{{ m.name }}</span>
                  <span class="provider-option-meta">{{ m.id }}</span>
                </div>
              </el-option>
            </el-select>
            <el-input
              v-else
              v-model="newProvider.defaultModel"
              :placeholder="$t('config.providers.modelPlaceholder')"
            />
            <div v-if="modelsUnavailable" class="form-item-help">
              {{ $t('config.providers.modelsFallback') }}
            </div>
          </el-form-item>
        </el-col>
        <el-col :span="24">
          <el-form-item :label="$t('config.providers.apiBaseUrl')" prop="apiBaseUrl">
            <el-input
              v-model="newProvider.apiBaseUrl"
              :placeholder="$t('config.providers.apiBasePlaceholder')"
            />
          </el-form-item>
        </el-col>
        <el-col :span="24">
          <el-form-item :label="$t('config.providers.apiKey')" prop="apiKey">
            <el-input v-model="newProvider.apiKey" show-password :placeholder="apiKeyPlaceholder" />
          </el-form-item>
        </el-col>
        <el-col :span="12">
          <el-form-item :label="$t('config.providers.maxTokens')">
            <el-input-number
              v-model="newProvider.maxTokens"
              :min="128"
              :max="8192"
              :step="128"
              style="width: 100%"
            />
          </el-form-item>
        </el-col>
        <el-col :span="12">
          <el-form-item :label="$t('config.providers.temperature')">
            <el-slider v-model="newProvider.temperature" :min="0" :max="2" :step="0.1" />
          </el-form-item>
        </el-col>
        <el-col :span="12">
          <el-form-item :label="$t('config.providers.timeoutShort')">
            <el-input-number
              v-model="newProvider.timeout"
              :min="5"
              :max="300"
              :step="5"
              style="width: 100%"
            />
          </el-form-item>
        </el-col>
        <el-col :span="12">
          <el-form-item :label="$t('config.providers.retryAttempts')">
            <el-input-number v-model="newProvider.retry" :min="0" :max="5" style="width: 100%" />
          </el-form-item>
        </el-col>
      </el-row>
    </el-form>
    <template #footer>
      <el-button @click="showAddProviderDialog = false">{{ $t('common.cancel') }}</el-button>
      <el-button type="primary" :loading="addingProvider" @click="confirmAddProvider">
        <el-icon><Plus /></el-icon>
        {{ $t('config.providers.addBtn') }}
      </el-button>
    </template>
  </el-dialog>
</template>

<script setup lang="ts">
import { computed, reactive, ref } from 'vue';
import { useI18n } from 'vue-i18n';
import { ArrowDown, Cpu, Delete, Plus } from '@element-plus/icons-vue';
import type { FormInstance } from 'element-plus';
import { PROVIDER_TYPES } from '../../types/llm';
import type { ProviderConfig, ProviderEntry } from '../../types/llm';
import type { CatalogModel, CatalogProvider } from '../../types/catalog';
import { createNewProvider, formatTemperature } from '../../composables/useProviders';
import { useCatalog } from '../../composables/useCatalog';
import { fetchCatalogModels } from '../../services/catalog';

defineProps<{
  /** Editable list of additional providers (persisted + newly added). */
  providers: ProviderEntry[];
  /** Whether the page is in edit mode. */
  isEditing: boolean;
  /** True while the provider list is being fetched. */
  loading: boolean;
}>();

const emit = defineEmits<{
  /** Expand/collapse a provider's inline form. */
  toggle: [index: number];
  /** Stage the provider at `index` for deletion (after confirmation). */
  remove: [index: number];
  /** Stage a validated new provider for the next save. */
  add: [payload: ProviderConfig];
}>();

const { t } = useI18n();

const showAddProviderDialog = ref(false);
const addingProvider = ref(false);
const addProviderFormRef = ref<FormInstance>();
const newProvider = reactive<ProviderConfig>(createNewProvider());

// --- models.dev catalog state (Add Provider dialog) ---
const { catalogProviders, catalogFailed, loadCatalogProviders } = useCatalog();
/** Catalog entry matching the dialog's selected provider (null for custom/preset). */
const selectedCatalogProvider = ref<CatalogProvider | null>(null);
/** Models offered for the selected catalog provider. */
const catalogModels = ref<CatalogModel[]>([]);
const modelsLoading = ref(false);
/** True when the models list could not be loaded — fall back to free-text input. */
const modelsUnavailable = ref(false);
// Sequence guard so a slow response for a previously-selected provider never
// overwrites the models of the current selection.
let modelsRequestSeq = 0;

/** Catalog options are used once the fetch returned a non-empty list. */
const catalogAvailable = computed(() => catalogProviders.value.length > 0);
/** Preset fallback list; `custom` is rendered separately as the first option. */
const presetProviderTypes = computed(() => PROVIDER_TYPES.filter((pt) => pt.value !== 'custom'));
/** API key placeholder: the catalog's env var name when known, else the default. */
const apiKeyPlaceholder = computed(
  () => selectedCatalogProvider.value?.env?.[0] || t('config.providers.apiKeyPlaceholder')
);
/** Show the model picker only for a catalog provider whose models loaded. */
const modelSelectActive = computed(
  () => selectedCatalogProvider.value != null && !modelsUnavailable.value
);

function openAddProviderDialog() {
  Object.assign(newProvider, createNewProvider());
  selectedCatalogProvider.value = null;
  catalogModels.value = [];
  modelsLoading.value = false;
  modelsUnavailable.value = false;
  // Invalidate any in-flight models request from a previous dialog session.
  modelsRequestSeq++;
  showAddProviderDialog.value = true;
  // Force a retry when the previous attempt failed — a 503 may be transient.
  void loadCatalogProviders(catalogFailed.value);
}

/** Handle the dialog's provider select: auto-fill from the catalog entry. */
function onDialogProviderChange(value: string) {
  const entry = catalogProviders.value.find((p) => p.id === value) ?? null;
  selectedCatalogProvider.value = entry;
  catalogModels.value = [];
  modelsUnavailable.value = false;
  if (!entry) return; // 'custom' or a preset fallback — fully manual entry.
  newProvider.apiBaseUrl = entry.api_base;
  void loadCatalogModelOptions(entry.id);
}

/** Load the catalog model list for the given provider, with race protection. */
async function loadCatalogModelOptions(providerId: string) {
  const seq = ++modelsRequestSeq;
  modelsLoading.value = true;
  try {
    const resp = await fetchCatalogModels(providerId);
    if (seq !== modelsRequestSeq) return; // superseded by a newer selection
    catalogModels.value = resp.models ?? [];
    // An empty list offers nothing to pick — fall back to free-text input.
    modelsUnavailable.value = catalogModels.value.length === 0;
  } catch {
    if (seq !== modelsRequestSeq) return;
    catalogModels.value = [];
    modelsUnavailable.value = true;
  } finally {
    if (seq === modelsRequestSeq) modelsLoading.value = false;
  }
}

async function confirmAddProvider() {
  if (!addProviderFormRef.value) return;
  const valid = await addProviderFormRef.value.validate().catch(() => false);
  if (!valid) return;

  addingProvider.value = true;
  try {
    emit('add', { ...(newProvider as ProviderConfig) });
    showAddProviderDialog.value = false;
  } finally {
    addingProvider.value = false;
  }
}
</script>

<style scoped>
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

.header-action {
  margin-left: auto;
}

.additional-providers-card :deep(.el-card__body) {
  padding: 16px 20px;
}

.providers-list {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.provider-item {
  border: 1px solid var(--border-color);
  border-radius: var(--radius-md);
  background: var(--bg-surface);
  overflow: hidden;
  transition: border-color 0.2s ease, box-shadow 0.2s ease;
}

.provider-item:hover {
  border-color: var(--brand);
  box-shadow: 0 0 0 1px var(--brand);
}

.provider-item.is-expanded {
  border-color: var(--brand);
  box-shadow: 0 0 0 1px var(--brand);
}

.provider-item-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 12px 16px;
  cursor: pointer;
  gap: 12px;
  user-select: none;
}

.provider-item-header:hover {
  background: rgba(var(--brand-rgb, 64, 158, 255), 0.04);
}

.provider-item-info {
  display: flex;
  align-items: center;
  gap: 10px;
  flex: 1;
  min-width: 0;
  overflow: hidden;
}

.provider-item-model {
  font-size: 13px;
  font-weight: 500;
  color: var(--text-primary);
  white-space: nowrap;
}

.provider-item-base {
  font-size: 12px;
  color: var(--text-secondary);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.provider-item-actions {
  display: flex;
  align-items: center;
  gap: 4px;
  flex-shrink: 0;
}

.provider-item-actions .el-icon {
  transition: transform 0.2s ease;
  color: var(--text-secondary);
}

.provider-item-actions .el-icon.rotated-open {
  transform: rotate(180deg);
}

.provider-item-body {
  padding: 16px;
  border-top: 1px solid var(--border-color);
  animation: slideDown 0.2s ease;
}

.provider-item-body.readonly-body {
  padding: 12px 16px;
}

@keyframes slideDown {
  from {
    opacity: 0;
    max-height: 0;
  }
  to {
    opacity: 1;
    max-height: 600px;
  }
}

.provider-item-body :deep(.el-form-item) {
  margin-bottom: 12px;
}

.provider-item-body :deep(.el-form-item__label) {
  font-size: 12px;
  padding-bottom: 4px;
}

.provider-item-body :deep(.el-slider) {
  margin-top: 8px;
}

.provider-item-body :deep(.el-descriptions__label) {
  font-size: 12px;
  color: var(--text-secondary);
}

.provider-item-body :deep(.el-descriptions__content) {
  font-size: 13px;
}

.provider-item-badge {
  padding: 6px 16px 10px;
  display: flex;
  align-items: center;
}

/* Helper text below form inputs (doc link, model fallback hint) */
.form-item-help {
  font-size: 12px;
  color: var(--text-secondary);
  margin-top: 6px;
  line-height: 1.4;
}

/* Catalog fallback warning at the top of the Add Provider dialog */
.catalog-fallback-alert {
  margin-bottom: 16px;
}

/* Provider/model dropdown option rows with secondary info on the right */
.provider-option {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
}

.provider-option-meta {
  font-size: 12px;
  color: var(--text-secondary);
}

/* Add provider dialog */
:deep(.el-dialog__body) {
  padding-top: 12px;
}

@media (max-width: 767px) {
  .card-body {
    padding: 16px;
  }
}
</style>
