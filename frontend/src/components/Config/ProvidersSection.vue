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
      <el-row :gutter="20">
        <el-col :span="12">
          <el-form-item :label="$t('config.providers.providerType')" prop="provider">
            <el-select v-model="newProvider.provider" style="width: 100%">
              <el-option
                v-for="pt in PROVIDER_TYPES"
                :key="pt.value"
                :label="pt.label"
                :value="pt.value"
              />
            </el-select>
          </el-form-item>
        </el-col>
        <el-col :span="12">
          <el-form-item :label="$t('config.providers.defaultModel')">
            <el-input
              v-model="newProvider.defaultModel"
              :placeholder="$t('config.providers.modelPlaceholder')"
            />
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
            <el-input
              v-model="newProvider.apiKey"
              show-password
              :placeholder="$t('config.providers.apiKeyPlaceholder')"
            />
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
import { reactive, ref } from 'vue';
import { ArrowDown, Cpu, Delete, Plus } from '@element-plus/icons-vue';
import type { FormInstance } from 'element-plus';
import { PROVIDER_TYPES } from '../../types/llm';
import type { ProviderConfig, ProviderEntry } from '../../types/llm';
import { createNewProvider, formatTemperature } from '../../composables/useProviders';

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

const showAddProviderDialog = ref(false);
const addingProvider = ref(false);
const addProviderFormRef = ref<FormInstance>();
const newProvider = reactive<ProviderConfig>(createNewProvider());

function openAddProviderDialog() {
  Object.assign(newProvider, createNewProvider());
  showAddProviderDialog.value = true;
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
