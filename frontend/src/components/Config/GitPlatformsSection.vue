<template>
  <!-- Git Platforms Card -->
  <el-card class="config-card git-platforms-card">
    <template #header>
      <div class="card-header">
        <el-icon><Connection /></el-icon>
        <span>{{ $t('config.gitPlatforms.title') }}</span>
        <div v-if="isEditing" class="header-action">
          <el-button size="small" type="primary" @click="openAddDialog">
            <el-icon><Plus /></el-icon>
            {{ $t('config.gitPlatforms.addBtn') }}
          </el-button>
        </div>
      </div>
    </template>
    <div class="card-body">
      <p class="card-subtitle">{{ $t('config.gitPlatforms.subtitle') }}</p>
      <el-empty
        v-if="platforms.length === 0"
        :description="$t('config.gitPlatforms.empty')"
        :image-size="80"
      >
        <el-button v-if="isEditing" size="small" type="primary" @click="openAddDialog">
          <el-icon><Plus /></el-icon>
          {{ $t('config.gitPlatforms.addBtn') }}
        </el-button>
      </el-empty>
      <div v-else class="platforms-list">
        <div v-for="(platform, index) in platforms" :key="platform.name" class="platform-item">
          <div class="platform-item-header">
            <div class="platform-item-info">
              <el-tag size="small">{{ platform.type }}</el-tag>
              <span class="platform-item-name">{{ platform.name }}</span>
              <span class="platform-item-base">{{ platform.baseUrl }}</span>
              <span v-if="platform.token" class="platform-item-token is-set">••••••••</span>
              <span v-else class="platform-item-token">{{ $t('config.notSet') }}</span>
            </div>
            <div class="platform-item-actions">
              <el-button
                size="small"
                text
                :loading="testingIndex === index"
                @click="testPlatform(index)"
              >
                {{ $t('config.gitPlatforms.test') }}
              </el-button>
              <template v-if="isEditing">
                <el-button size="small" text @click="openEditDialog(index)">
                  {{ $t('common.edit') }}
                </el-button>
                <el-button size="small" text type="danger" @click="confirmRemove(index)">
                  <el-icon><Delete /></el-icon>
                </el-button>
              </template>
            </div>
          </div>
        </div>
      </div>
    </div>
  </el-card>

  <!-- Add / Edit Git Platform Dialog -->
  <el-dialog
    v-model="showDialog"
    :title="
      dialogMode === 'add'
        ? $t('config.gitPlatforms.addDialogTitle')
        : $t('config.gitPlatforms.editDialogTitle')
    "
    width="640px"
    append-to-body
  >
    <el-form
      ref="dialogFormRef"
      :model="draft"
      :rules="dialogRules"
      label-position="top"
      size="default"
    >
      <el-row :gutter="20">
        <el-col :span="12">
          <el-form-item :label="$t('config.gitPlatforms.name')" prop="name">
            <el-input
              v-model="draft.name"
              :placeholder="$t('config.gitPlatforms.namePlaceholder')"
            />
          </el-form-item>
        </el-col>
        <el-col :span="12">
          <!-- Single-option select today; gitea/gitee slot in as options later. -->
          <el-form-item :label="$t('config.gitPlatforms.type')" prop="type">
            <el-select v-model="draft.type" style="width: 100%">
              <el-option label="GitLab" value="gitlab" />
            </el-select>
          </el-form-item>
        </el-col>
        <el-col :span="24">
          <el-form-item :label="$t('config.gitPlatforms.baseUrl')" prop="baseUrl">
            <el-input
              v-model="draft.baseUrl"
              :placeholder="$t('config.gitPlatforms.baseUrlPlaceholder')"
            />
          </el-form-item>
        </el-col>
        <el-col :span="24">
          <el-form-item :label="$t('config.gitPlatforms.token')" prop="token">
            <el-input
              v-model="draft.token"
              show-password
              :placeholder="
                dialogMode === 'edit'
                  ? $t('config.gitPlatforms.keepTokenPlaceholder')
                  : $t('config.gitPlatforms.tokenPlaceholder')
              "
            />
          </el-form-item>
        </el-col>
        <el-col :span="24">
          <el-form-item :label="$t('config.gitPlatforms.webhookSecret')" prop="webhookSecret">
            <el-input
              v-model="draft.webhookSecret"
              show-password
              :placeholder="
                dialogMode === 'edit'
                  ? $t('config.gitPlatforms.keepSecretPlaceholder')
                  : $t('common.optional')
              "
            />
            <div class="form-item-help">{{ $t('config.gitPlatforms.webhookSecretHelp') }}</div>
          </el-form-item>
        </el-col>
        <el-col :span="24">
          <el-form-item
            :label="$t('config.gitPlatforms.webhookSigningSecret')"
            prop="webhookSigningSecret"
          >
            <el-input
              v-model="draft.webhookSigningSecret"
              show-password
              :placeholder="
                dialogMode === 'edit'
                  ? $t('config.gitPlatforms.keepSecretPlaceholder')
                  : $t('common.optional')
              "
            />
            <div class="form-item-help">{{ $t('config.gitPlatforms.webhookSigningSecretHelp') }}</div>
          </el-form-item>
        </el-col>
      </el-row>
    </el-form>
    <template #footer>
      <el-button @click="showDialog = false">{{ $t('common.cancel') }}</el-button>
      <el-button type="primary" :loading="savingDialog" @click="confirmDialog">
        <el-icon v-if="dialogMode === 'add'"><Plus /></el-icon>
        {{ dialogMode === 'add' ? $t('config.gitPlatforms.addBtn') : $t('common.save') }}
      </el-button>
    </template>
  </el-dialog>
</template>

<script setup lang="ts">
import { computed, reactive, ref } from 'vue';
import { useI18n } from 'vue-i18n';
import { Connection, Delete, Plus } from '@element-plus/icons-vue';
import { ElMessage, ElMessageBox, type FormInstance, type FormRules } from 'element-plus';
import type { GitPlatformConfig } from '../../types/config';
import { testGitPlatform } from '../../services/config';

const props = defineProps<{
  /** Configured git platform entries (secrets masked as returned by GET /config). */
  platforms: GitPlatformConfig[];
  /** Whether the page is in edit mode. */
  isEditing: boolean;
}>();

const emit = defineEmits<{
  /** Stage a new platform entry for the next save. */
  add: [entry: GitPlatformConfig];
  /** Replace the entry at `index` for the next save. */
  edit: [index: number, entry: GitPlatformConfig];
  /** Drop the entry at `index` for the next save. */
  remove: [index: number];
}>();

const { t } = useI18n();

// --- Add / Edit dialog state ---
const showDialog = ref(false);
const savingDialog = ref(false);
const dialogMode = ref<'add' | 'edit'>('add');
/** Index of the row being edited; -1 when adding. */
const editingIndex = ref(-1);
const dialogFormRef = ref<FormInstance>();
const draft = reactive<GitPlatformConfig>({
  name: '',
  type: 'gitlab',
  baseUrl: '',
  token: '',
  webhookSecret: '',
  webhookSigningSecret: '',
});

// --- Test state ---
/** Row whose connectivity probe is in flight (null when idle). */
const testingIndex = ref<number | null>(null);

function blankDraft(): GitPlatformConfig {
  return {
    name: '',
    type: 'gitlab',
    baseUrl: '',
    token: '',
    webhookSecret: '',
    webhookSigningSecret: '',
  };
}

function openAddDialog() {
  Object.assign(draft, blankDraft());
  dialogMode.value = 'add';
  editingIndex.value = -1;
  showDialog.value = true;
}

function openEditDialog(index: number) {
  const platform = props.platforms[index];
  // Secret fields start blank: the backend keeps the stored secret for a
  // matching name when the submitted value is empty or the `***` mask, so a
  // blank field here means "leave unchanged" (see the placeholder text).
  Object.assign(draft, {
    name: platform.name,
    type: platform.type,
    baseUrl: platform.baseUrl,
    token: '',
    webhookSecret: '',
    webhookSigningSecret: '',
  });
  dialogMode.value = 'edit';
  editingIndex.value = index;
  showDialog.value = true;
}

/** Name must be unique across rows (excluding the row being edited). */
function validateUniqueName(_rule: unknown, value: string, callback: (error?: Error) => void) {
  const name = (value ?? '').trim();
  const duplicated = props.platforms.some((p, i) => i !== editingIndex.value && p.name === name);
  if (duplicated) {
    callback(new Error(t('config.gitPlatforms.nameDuplicate')));
  } else {
    callback();
  }
}

function validateUrl(_rule: unknown, value: string, callback: (error?: Error) => void) {
  try {
    new URL(value);
    callback();
  } catch {
    callback(new Error(t('config.validation.invalidUrl')));
  }
}

const dialogRules = computed<FormRules>(() => ({
  name: [
    { required: true, message: t('config.gitPlatforms.nameRequired'), trigger: 'blur' },
    { validator: validateUniqueName, trigger: 'blur' },
  ],
  baseUrl: [
    { required: true, message: t('config.gitPlatforms.baseUrlRequired'), trigger: 'blur' },
    { validator: validateUrl, trigger: 'blur' },
  ],
}));

async function confirmDialog() {
  if (!dialogFormRef.value) return;
  const valid = await dialogFormRef.value.validate().catch(() => false);
  if (!valid) return;

  savingDialog.value = true;
  try {
    const entry: GitPlatformConfig = {
      name: draft.name.trim(),
      type: draft.type,
      // Strip trailing slashes so the stored entry matches the server-side
      // probe's normalized form (the masked-token fallback matches on the
      // exact baseUrl string).
      baseUrl: draft.baseUrl.trim().replace(/\/+$/, ''),
      token: draft.token,
      webhookSecret: draft.webhookSecret,
      webhookSigningSecret: draft.webhookSigningSecret,
    };
    if (dialogMode.value === 'edit') {
      const original = props.platforms[editingIndex.value];
      // A secret left blank while editing carries over the previously loaded
      // value (the `***` mask or empty), so the row display keeps reporting
      // the correct configured state and the PUT keeps the stored secret.
      if (!entry.token) entry.token = original.token;
      if (!entry.webhookSecret) entry.webhookSecret = original.webhookSecret;
      if (!entry.webhookSigningSecret) entry.webhookSigningSecret = original.webhookSigningSecret;
      emit('edit', editingIndex.value, entry);
    } else {
      emit('add', entry);
    }
    showDialog.value = false;
  } finally {
    savingDialog.value = false;
  }
}

/**
 * Probe a platform's connectivity. The row's token is sent as-is: a masked
 * (`***`) or blank token falls back server-side to the stored token of the
 * platform with the matching baseUrl. The endpoint always answers HTTP 200,
 * so probe failures arrive in the body; only network/HTTP errors hit catch.
 */
async function testPlatform(index: number) {
  const platform = props.platforms[index];
  testingIndex.value = index;
  try {
    const result = await testGitPlatform({ baseUrl: platform.baseUrl, token: platform.token });
    if (result.ok) {
      ElMessage.success(t('config.gitPlatforms.testOk', { version: result.version ?? '?' }));
    } else {
      ElMessage.error(
        t('config.gitPlatforms.testFailed', { error: result.error ?? t('errors.unknown') })
      );
    }
  } catch (e) {
    ElMessage.error(
      t('config.gitPlatforms.testFailed', { error: e instanceof Error ? e.message : String(e) })
    );
  } finally {
    testingIndex.value = null;
  }
}

/** Ask for confirmation, then stage the row for deletion on save. */
function confirmRemove(index: number) {
  ElMessageBox.confirm(
    t('config.gitPlatforms.removeConfirm', { name: props.platforms[index].name }),
    t('config.gitPlatforms.removeTitle'),
    {
      confirmButtonText: t('common.remove'),
      cancelButtonText: t('common.cancel'),
      type: 'warning',
    }
  )
    .then(() => emit('remove', index))
    .catch(() => {
      /* cancelled */
    });
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

.git-platforms-card :deep(.el-card__body) {
  padding: 16px 20px;
}

.card-subtitle {
  font-size: 12px;
  color: var(--text-secondary);
  line-height: 1.4;
  margin: 0 0 16px;
}

.platforms-list {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.platform-item {
  border: 1px solid var(--border-color);
  border-radius: var(--radius-md);
  background: var(--bg-surface);
  overflow: hidden;
  transition: border-color 0.2s ease, box-shadow 0.2s ease;
}

.platform-item:hover {
  border-color: var(--brand);
  box-shadow: 0 0 0 1px var(--brand);
}

.platform-item-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 12px 16px;
  gap: 12px;
}

.platform-item-info {
  display: flex;
  align-items: center;
  gap: 10px;
  flex: 1;
  min-width: 0;
  overflow: hidden;
}

.platform-item-name {
  font-size: 13px;
  font-weight: 500;
  color: var(--text-primary);
  white-space: nowrap;
}

.platform-item-base {
  font-size: 12px;
  color: var(--text-secondary);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.platform-item-token {
  font-size: 12px;
  color: var(--text-secondary);
  white-space: nowrap;
  flex-shrink: 0;
}

.platform-item-token.is-set {
  font-family: var(--font-mono);
  letter-spacing: 2px;
}

.platform-item-actions {
  display: flex;
  align-items: center;
  gap: 4px;
  flex-shrink: 0;
}

/* Helper text below form inputs (webhook secret hint) */
.form-item-help {
  font-size: 12px;
  color: var(--text-secondary);
  margin-top: 6px;
  line-height: 1.4;
}

:deep(.el-dialog__body) {
  padding-top: 12px;
}

@media (max-width: 767px) {
  .card-body {
    padding: 16px;
  }
}
</style>
