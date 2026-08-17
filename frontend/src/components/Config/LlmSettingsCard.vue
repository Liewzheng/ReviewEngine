<template>
  <!-- LLM Card -->
  <el-card class="config-card">
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
            <el-input
              v-model="config.apiBaseUrl"
              :disabled="!isEditing"
              :placeholder="$t('config.llm.apiBasePlaceholder')"
            />
          </el-form-item>
        </el-col>
        <el-col :xs="24" :sm="12">
          <el-form-item :label="$t('config.llm.apiKey')" prop="llm.openaiApiKey">
            <el-input
              v-model="config.openaiApiKey"
              :disabled="!isEditing"
              show-password
              :placeholder="$t('config.llm.apiKeyPlaceholder')"
            />
          </el-form-item>
        </el-col>
        <el-col :xs="24" :sm="12">
          <el-form-item :label="$t('config.llm.defaultModel')" prop="llm.defaultModel">
            <el-select
              v-model="config.defaultModel"
              :disabled="!isEditing"
              :loading="modelFetchLoading"
              :placeholder="$t('config.llm.selectModelPlaceholder')"
              style="width: 100%"
            >
              <el-option v-for="model in models" :key="model" :label="model" :value="model" />
            </el-select>
            <div v-if="modelFetchError" class="form-item-help error-text">{{ modelFetchError }}</div>
          </el-form-item>
        </el-col>
        <el-col :xs="24" :sm="12">
          <el-form-item :label="$t('config.llm.maxTokens')" prop="llm.maxTokens">
            <el-input-number
              v-model="config.maxTokens"
              :disabled="!isEditing"
              :min="128"
              :max="8192"
              :step="128"
              style="width: 100%"
            />
          </el-form-item>
        </el-col>
        <el-col :xs="24" :sm="12">
          <el-form-item :label="$t('config.llm.temperature')" prop="llm.temperature">
            <div class="slider-with-value">
              <el-slider v-model="config.temperature" :disabled="!isEditing" :min="0" :max="2" :step="0.1" />
              <span class="slider-value">{{ config.temperature }}</span>
            </div>
          </el-form-item>
        </el-col>
        <el-col :xs="24" :sm="12">
          <el-form-item :label="$t('config.llm.timeout')" prop="llm.timeoutSeconds">
            <el-input-number
              v-model="config.timeoutSeconds"
              :disabled="!isEditing"
              :min="5"
              :max="300"
              :step="5"
              style="width: 100%"
            />
          </el-form-item>
        </el-col>
        <el-col :xs="24" :sm="12">
          <el-form-item :label="$t('config.llm.retryAttempts')" prop="llm.retryAttempts">
            <el-input-number
              v-model="config.retryAttempts"
              :disabled="!isEditing"
              :min="0"
              :max="5"
              style="width: 100%"
            />
          </el-form-item>
        </el-col>
      </el-row>
      <div class="test-connection">
        <el-button :loading="testing" @click="emit('test')">
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
    </div>
  </el-card>
</template>

<script setup lang="ts">
import { Connection, Cpu } from '@element-plus/icons-vue';
import type { LLMConfig } from '../../types/config';
import type { TestResult } from '../../types/llm';

defineProps<{
  /** The reactive LLM section of the main config form. */
  config: LLMConfig;
  /** Whether the page is in edit mode. */
  isEditing: boolean;
  /** Model list fetched from the configured endpoint. */
  models: string[];
  /** True while the model list is being fetched. */
  modelFetchLoading: boolean;
  /** Error message from the last model fetch attempt. */
  modelFetchError: string | null;
  /** True while a connection test is in progress. */
  testing: boolean;
  /** Result of the last connection test (null before first test). */
  testResult: TestResult | null;
}>();

const emit = defineEmits<{
  /** Run a connectivity test against the configured provider. */
  test: [];
}>();
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

@media (max-width: 767px) {
  .card-body {
    padding: 16px;
  }

  :deep(.el-slider) {
    width: 100%;
  }
}
</style>
