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
              <!-- The connectivity probe is a read-only check, so it must stay
                   clickable in view mode: an explicit :disabled="false" short-
                   circuits the disabled injected by the surrounding el-form
                   (useFormDisabled: component prop wins over form context). -->
              <el-button
                size="small"
                text
                :loading="testingIndex === index"
                :disabled="false"
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
          <el-form-item prop="internalBaseUrl">
            <template #label>
              {{ $t('config.gitPlatforms.internalBaseUrl') }}
              <HelpTip :tip="$t('config.gitPlatforms.internalBaseUrlHelp')" />
            </template>
            <el-input
              v-model="draft.internalBaseUrl"
              :placeholder="$t('config.gitPlatforms.internalBaseUrlPlaceholder')"
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
                  ? hasSavedToken
                    ? SAVED_SECRET_PLACEHOLDER
                    : $t('config.gitPlatforms.keepTokenPlaceholder')
                  : $t('config.gitPlatforms.tokenPlaceholder')
              "
            />
          </el-form-item>
        </el-col>
        <!-- Field order mirrors the GitLab 19+ webhook form: Signing token
             (recommended) before Secret token (optional fallback). -->
        <el-col :span="24">
          <el-form-item prop="webhookSigningSecret">
            <template #label>
              {{ $t('config.gitPlatforms.webhookSigningSecret') }}
              <HelpTip :tip="$t('config.gitPlatforms.webhookSigningSecretHelp')" />
            </template>
            <el-input
              v-model="draft.webhookSigningSecret"
              show-password
              :placeholder="
                dialogMode === 'edit'
                  ? hasSavedWebhookSigningSecret
                    ? SAVED_SECRET_PLACEHOLDER
                    : $t('config.gitPlatforms.keepSecretPlaceholder')
                  : $t('common.optional')
              "
            />
          </el-form-item>
        </el-col>
        <el-col :span="24">
          <el-form-item prop="webhookSecret">
            <template #label>
              {{ $t('config.gitPlatforms.webhookSecret') }}
              <HelpTip :tip="$t('config.gitPlatforms.webhookSecretHelp')" />
            </template>
            <el-input
              v-model="draft.webhookSecret"
              show-password
              :placeholder="
                dialogMode === 'edit'
                  ? hasSavedWebhookSecret
                    ? SAVED_SECRET_PLACEHOLDER
                    : $t('config.gitPlatforms.keepSecretPlaceholder')
                  : $t('common.optional')
              "
            />
          </el-form-item>
        </el-col>
        <el-col :span="24">
          <el-form-item prop="allowedProjects">
            <template #label>
              {{ $t('config.gitPlatforms.allowedProjects') }}
              <HelpTip :tip="$t('config.gitPlatforms.allowedProjectsHelp')" />
            </template>
            <el-input
              v-model="draft.allowedProjectsText"
              type="textarea"
              :rows="3"
              resize="vertical"
              :placeholder="$t('config.gitPlatforms.allowedProjectsPlaceholder')"
            />
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
import { computed, h, reactive, ref, type FunctionalComponent } from 'vue';
import { useI18n } from 'vue-i18n';
import { Connection, Delete, InfoFilled, Plus } from '@element-plus/icons-vue';
import {
  ElIcon,
  ElMessage,
  ElMessageBox,
  ElTooltip,
  type FormInstance,
  type FormRules,
} from 'element-plus';
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
/** Draft state for the add/edit dialog: the `GitPlatformConfig` contract plus
 * the newline-joined textarea mirror of `allowedProjects`. */
type GitPlatformDraft = GitPlatformConfig & { allowedProjectsText: string };
const draft = reactive<GitPlatformDraft>({
  name: '',
  type: 'gitlab',
  baseUrl: '',
  internalBaseUrl: '',
  token: '',
  webhookSecret: '',
  webhookSigningSecret: '',
  allowedProjects: [],
  allowedProjectsText: '',
});

/** Placeholder shown for a secret that already has a stored value. */
const SAVED_SECRET_PLACEHOLDER = '••••••••••';

/** 与后端 API_KEY_MASK（src/server/api/config/types.rs）的契约：已保存密钥以掩码返回 */
const SECRET_MASK = '***';

// GET /config masks saved secrets as the SECRET_MASK literal (unsaved = '').
// openEditDialog blanks the draft but keeps the masked value in
// props.platforms[editingIndex], so SECRET_MASK there means "this field is stored".
const hasSavedSecret = (field: string | undefined) => field === SECRET_MASK;

const hasSavedToken = computed(
  () => dialogMode.value === 'edit' && hasSavedSecret(props.platforms[editingIndex.value]?.token)
);
const hasSavedWebhookSecret = computed(
  () =>
    dialogMode.value === 'edit' &&
    hasSavedSecret(props.platforms[editingIndex.value]?.webhookSecret)
);
const hasSavedWebhookSigningSecret = computed(
  () =>
    dialogMode.value === 'edit' &&
    hasSavedSecret(props.platforms[editingIndex.value]?.webhookSigningSecret)
);

/**
 * ⓘ help tooltip shown next to a form label. Focusable (tabindex=0) and
 * triggered by both hover and focus, so keyboard users can reveal it too.
 */
const HelpTip: FunctionalComponent<{ tip: string }> = (props) =>
  h(
    ElTooltip,
    { content: props.tip, placement: 'top', trigger: ['hover', 'focus'] },
    {
      default: () =>
        h(
          ElIcon,
          { class: 'help-icon', tabindex: 0, 'aria-label': props.tip },
          { default: () => h(InfoFilled) }
        ),
    }
  );
HelpTip.props = ['tip'];

// --- Test state ---
/** Row whose connectivity probe is in flight (null when idle). */
const testingIndex = ref<number | null>(null);

function blankDraft(): GitPlatformDraft {
  return {
    name: '',
    type: 'gitlab',
    baseUrl: '',
    internalBaseUrl: '',
    token: '',
    webhookSecret: '',
    webhookSigningSecret: '',
    allowedProjects: [],
    allowedProjectsText: '',
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
  // allowedProjects round-trips through the textarea as one path per line;
  // an empty (or previously empty) list shows an empty textarea (= all).
  Object.assign(draft, {
    name: platform.name,
    type: platform.type,
    baseUrl: platform.baseUrl,
    // internalBaseUrl is plain config, never masked: empty means "fall back to
    // baseUrl", so it round-trips as-is (old entries may lack the field).
    internalBaseUrl: platform.internalBaseUrl ?? '',
    token: '',
    webhookSecret: '',
    webhookSigningSecret: '',
    allowedProjects: platform.allowedProjects ?? [],
    allowedProjectsText: (platform.allowedProjects ?? []).join('\n'),
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
      // Internal URL is submitted trimmed but otherwise verbatim: the backend
      // treats it as "reng's reachable address", empty = fall back to baseUrl.
      internalBaseUrl: draft.internalBaseUrl.trim(),
      token: draft.token,
      webhookSecret: draft.webhookSecret,
      webhookSigningSecret: draft.webhookSigningSecret,
      // One project path per line; trim whitespace, drop blank lines, and
      // keep the first occurrence of each path. Empty result = all projects.
      allowedProjects: Array.from(
        new Set(
          draft.allowedProjectsText
            .split('\n')
            .map((line) => line.trim())
            .filter(Boolean)
        )
      ),
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

/* Info icon next to form labels; hover/focus reveals the tooltip */
.help-icon {
  margin-left: 4px;
  font-size: 14px;
  vertical-align: text-bottom;
  color: var(--el-text-color-secondary);
  cursor: help;
}

.help-icon:focus-visible {
  outline: 2px solid var(--el-color-primary);
  outline-offset: 1px;
  border-radius: 50%;
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
