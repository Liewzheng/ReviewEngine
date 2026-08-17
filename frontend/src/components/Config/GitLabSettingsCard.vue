<template>
  <!-- GitLab Card -->
  <el-card class="config-card">
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
            <el-input
              v-model="config.url"
              :disabled="!isEditing"
              :placeholder="$t('config.gitlab.urlPlaceholder')"
            />
          </el-form-item>
        </el-col>
        <el-col :xs="24" :sm="12">
          <el-form-item :label="$t('header.apiToken')" prop="gitlab.apiToken">
            <div v-if="!isEditing" class="readonly-field">
              <template v-if="!config.apiToken">
                <span class="empty-text">{{ $t('config.notSet') }}</span>
              </template>
              <template v-else-if="!revealed.apiToken">
                <span class="masked-text">••••••••••••</span>
                <el-button
                  size="small"
                  :aria-label="$t('config.gitlab.revealApiTokenAria')"
                  @click.stop="emit('reveal', 'apiToken')"
                >
                  <el-icon><View /></el-icon>
                </el-button>
              </template>
              <template v-else>
                <span class="revealed-value">{{ config.apiToken }}</span>
                <span class="countdown">
                  {{ $t('config.revealCountdown', { count: revealCountdown.apiToken }) }}
                </span>
              </template>
            </div>
            <el-input
              v-else
              v-model="config.apiToken"
              :disabled="!isEditing"
              show-password
              :placeholder="$t('config.gitlab.apiTokenPlaceholder')"
            />
          </el-form-item>
        </el-col>
        <el-col :xs="24" :sm="12">
          <el-form-item :label="$t('config.gitlab.webhookSecret')" prop="gitlab.webhookSecret">
            <div v-if="!isEditing" class="readonly-field">
              <template v-if="!config.webhookSecret">
                <span class="empty-text">{{ $t('config.notSet') }}</span>
              </template>
              <template v-else-if="!revealed.webhookSecret">
                <span class="masked-text">••••••••••••</span>
                <el-button
                  size="small"
                  :aria-label="$t('config.gitlab.revealWebhookAria')"
                  @click.stop="emit('reveal', 'webhookSecret')"
                >
                  <el-icon><View /></el-icon>
                </el-button>
              </template>
              <template v-else>
                <span class="revealed-value">{{ config.webhookSecret }}</span>
                <span class="countdown">
                  {{ $t('config.revealCountdown', { count: revealCountdown.webhookSecret }) }}
                </span>
              </template>
            </div>
            <el-input
              v-else
              v-model="config.webhookSecret"
              :disabled="!isEditing"
              show-password
              :placeholder="$t('common.optional')"
            />
          </el-form-item>
        </el-col>
        <el-col :xs="24" :sm="12">
          <el-form-item
            :label="$t('config.gitlab.webhookSigningSecret')"
            prop="gitlab.webhookSigningSecret"
          >
            <div v-if="!isEditing" class="readonly-field">
              <template v-if="!config.webhookSigningSecret">
                <span class="empty-text">{{ $t('config.notSet') }}</span>
              </template>
              <template v-else-if="!revealed.webhookSigningSecret">
                <span class="masked-text">••••••••••••</span>
                <el-button
                  size="small"
                  :aria-label="$t('config.gitlab.revealSigningAria')"
                  @click.stop="emit('reveal', 'webhookSigningSecret')"
                >
                  <el-icon><View /></el-icon>
                </el-button>
              </template>
              <template v-else>
                <span class="revealed-value">{{ config.webhookSigningSecret }}</span>
                <span class="countdown">
                  {{ $t('config.revealCountdown', { count: revealCountdown.webhookSigningSecret }) }}
                </span>
              </template>
            </div>
            <el-input
              v-else
              v-model="config.webhookSigningSecret"
              :disabled="!isEditing"
              show-password
              :placeholder="$t('config.gitlab.signingPlaceholder')"
            />
            <div v-if="isEditing" class="form-item-help">{{ $t('config.gitlab.signingHelp') }}</div>
          </el-form-item>
        </el-col>
        <el-col :xs="24" :sm="12">
          <el-form-item :label="$t('config.gitlab.defaultProject')" prop="gitlab.defaultProject">
            <el-input
              v-model="config.defaultProject"
              :disabled="!isEditing"
              clearable
              :placeholder="$t('config.gitlab.defaultProjectPlaceholder')"
            />
            <div v-if="isEditing" class="form-item-help">
              {{ $t('config.gitlab.defaultProjectHelp') }}
            </div>
          </el-form-item>
        </el-col>
        <el-col :xs="24" :sm="12">
          <el-form-item :label="$t('config.gitlab.mrLabel')" prop="gitlab.mrLabel">
            <el-input v-model="config.mrLabel" :disabled="!isEditing" placeholder="needs-review" />
          </el-form-item>
        </el-col>
        <el-col :xs="24" :sm="12">
          <el-form-item :label="$t('config.gitlab.autoReview')" prop="gitlab.autoReview">
            <el-switch v-model="config.autoReview" :disabled="!isEditing" />
          </el-form-item>
        </el-col>
      </el-row>
    </div>
  </el-card>
</template>

<script setup lang="ts">
import { Link, View } from '@element-plus/icons-vue';
import type { GitLabConfig } from '../../types/config';
import type { RevealableField } from '../../composables/useConfigForm';

defineProps<{
  /** The reactive GitLab section of the main config form. */
  config: GitLabConfig;
  /** Whether the page is in edit mode. */
  isEditing: boolean;
  /** Which secrets are currently revealed (read-only mode). */
  revealed: Record<RevealableField, boolean>;
  /** Seconds left before each revealed secret auto-hides. */
  revealCountdown: Record<RevealableField, number>;
}>();

const emit = defineEmits<{
  /** Temporarily reveal a secret in read-only mode. */
  reveal: [field: RevealableField];
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

@media (max-width: 767px) {
  .card-body {
    padding: 16px;
  }
}
</style>
