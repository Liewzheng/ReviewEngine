<template>
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
          <!-- RENG-96: a configured secret shows the `***` mask IN the box
               (not an empty box with a placeholder); typing over it replaces
               the stored value, the clear button removes it. -->
          <div class="secret-field">
            <el-input
              :model-value="secretDisplay(draft.token)"
              :show-password="showPasswordFor(draft.token)"
              :placeholder="secretPlaceholder('token')"
              @update:model-value="draft.token = $event"
              @focus="selectAllOnMask"
            />
            <el-button
              v-if="clearable(draft.token)"
              size="small"
              text
              type="warning"
              class="secret-clear-btn"
              :aria-label="t('config.gitPlatforms.clearSecret')"
              @click="clearSecret('token')"
            >
              {{ $t('config.gitPlatforms.clearSecret') }}
            </el-button>
          </div>
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
          <div class="secret-field">
            <el-input
              :model-value="secretDisplay(draft.webhookSigningSecret)"
              :show-password="showPasswordFor(draft.webhookSigningSecret)"
              :placeholder="secretPlaceholder('webhookSigningSecret')"
              @update:model-value="draft.webhookSigningSecret = $event"
              @focus="selectAllOnMask"
            />
            <el-button
              v-if="clearable(draft.webhookSigningSecret)"
              size="small"
              text
              type="warning"
              class="secret-clear-btn"
              :aria-label="t('config.gitPlatforms.clearSecret')"
              @click="clearSecret('webhookSigningSecret')"
            >
              {{ $t('config.gitPlatforms.clearSecret') }}
            </el-button>
          </div>
        </el-form-item>
      </el-col>
      <el-col :span="24">
        <el-form-item prop="webhookSecret">
          <template #label>
            {{ $t('config.gitPlatforms.webhookSecret') }}
            <HelpTip :tip="$t('config.gitPlatforms.webhookSecretHelp')" />
          </template>
          <div class="secret-field">
            <el-input
              :model-value="secretDisplay(draft.webhookSecret)"
              :show-password="showPasswordFor(draft.webhookSecret)"
              :placeholder="secretPlaceholder('webhookSecret')"
              @update:model-value="draft.webhookSecret = $event"
              @focus="selectAllOnMask"
            />
            <el-button
              v-if="clearable(draft.webhookSecret)"
              size="small"
              text
              type="warning"
              class="secret-clear-btn"
              :aria-label="t('config.gitPlatforms.clearSecret')"
              @click="clearSecret('webhookSecret')"
            >
              {{ $t('config.gitPlatforms.clearSecret') }}
            </el-button>
          </div>
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
</template>

<script setup lang="ts">
import { computed, h, reactive, ref, type FunctionalComponent } from 'vue';
import { useI18n } from 'vue-i18n';
import { InfoFilled } from '@element-plus/icons-vue';
import { ElIcon, ElTooltip, type FormInstance, type FormRules } from 'element-plus';
import type { GitPlatformConfig } from '../../types/config';
import { CLEAR_SECRET_SENTINEL } from '../../types/config';
import {
  clearable,
  editDraftSecrets,
  SECRET_MASK,
  secretDisplay,
  type SecretField,
  showPasswordFor,
  submittedSecret,
} from './gitPlatformDialogState';

const props = defineProps<{
  /** 'add' for a brand-new entry, 'edit' for an existing row. */
  mode: 'add' | 'edit';
  /** The row being edited; undefined in add mode. */
  platform?: GitPlatformConfig;
  /** Every configured row — the unique-name check excludes the edited one. */
  platforms: GitPlatformConfig[];
  /** Index of the edited row; -1 in add mode. */
  editingIndex: number;
}>();

const emit = defineEmits<{
  /** The validated entry; the parent stages it for the auto-save. */
  save: [entry: GitPlatformConfig];
  cancel: [];
}>();

const { t } = useI18n();

const dialogFormRef = ref<FormInstance>();
/** Draft state: the `GitPlatformConfig` contract plus the newline-joined
 * textarea mirror of `allowedProjects`. Secret fields hold the echoed `***`
 * mask when a value is stored (shown in the box), the clear sentinel after
 * an explicit clear, or the typed value. Initialized once — the dialog
 * shell mounts this form per open, so the draft starts fresh every time. */
type GitPlatformDraft = GitPlatformConfig & { allowedProjectsText: string };
const draft = reactive<GitPlatformDraft>({
  id: props.platform?.id ?? '',
  name: props.platform?.name ?? '',
  type: props.platform?.type ?? 'gitlab',
  baseUrl: props.platform?.baseUrl ?? '',
  internalBaseUrl: props.platform?.internalBaseUrl ?? '',
  // Secret fields take the echoed value: the `***` mask when configured —
  // so the box SHOWS the mask — or `''` when unset.
  ...editDraftSecrets(props.platform ?? ({} as GitPlatformConfig)),
  allowedProjects: props.platform?.allowedProjects ?? [],
  allowedProjectsText: (props.platform?.allowedProjects ?? []).join('\n'),
});

/** Placeholder for a secret input: only visible when the box is empty, which
 * now happens only for an unconfigured secret or after an explicit clear
 * (the mask fills the box for a configured one, so nothing is hinted). */
function secretPlaceholder(field: SecretField): string {
  if (draft[field] === SECRET_MASK) return '';
  if (props.mode === 'add') {
    return field === 'token'
      ? t('config.gitPlatforms.tokenPlaceholder')
      : t('common.optional');
  }
  if (draft[field] === CLEAR_SECRET_SENTINEL) {
    // The stored value was explicitly cleared: type a fresh one.
    return field === 'token'
      ? t('config.gitPlatforms.tokenPlaceholder')
      : t('common.optional');
  }
  // Empty box with (or without) a stored value: blank = keep.
  return field === 'token'
    ? t('config.gitPlatforms.keepTokenPlaceholder')
    : t('config.gitPlatforms.keepSecretPlaceholder');
}

/** Explicitly clear a stored secret: the sentinel travels to the backend,
 * which maps it to an empty stored value (blank/mask would mean "keep"). */
function clearSecret(field: SecretField) {
  draft[field] = CLEAR_SECRET_SENTINEL;
}

/** Typing over the visible mask replaces the whole value. */
function selectAllOnMask(e: FocusEvent) {
  const input = e.target as HTMLInputElement;
  if (input.value === SECRET_MASK) input.select();
}

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

/** Name must be unique across rows (excluding the row being edited). */
function validateUniqueName(_rule: unknown, value: string, callback: (error?: Error) => void) {
  const name = (value ?? '').trim();
  const duplicated = props.platforms.some((p, i) => i !== props.editingIndex && p.name === name);
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

  const original = props.platform;
  const entry: GitPlatformConfig = {
    id: draft.id || undefined,
    name: draft.name.trim(),
    type: draft.type,
    // Strip trailing slashes so the stored entry matches the server-side
    // probe's normalized form (the masked-token fallback matches on the
    // exact baseUrl string).
    baseUrl: draft.baseUrl.trim().replace(/\/+$/, ''),
    // Internal URL is submitted trimmed but otherwise verbatim: the backend
    // treats it as "reng's reachable address", empty = fall back to baseUrl.
    internalBaseUrl: draft.internalBaseUrl.trim(),
    token: submittedSecret(draft.token, original?.token ?? ''),
    webhookSecret: submittedSecret(draft.webhookSecret, original?.webhookSecret ?? ''),
    webhookSigningSecret: submittedSecret(
      draft.webhookSigningSecret,
      original?.webhookSigningSecret ?? ''
    ),
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
  emit('save', entry);
}

// The dialog shell's Save button triggers the same validated submission.
defineExpose({ save: confirmDialog });
</script>

<style scoped>
/* A secret input with its clear button on one row. */
.secret-field {
  display: flex;
  gap: var(--space-2);
  width: 100%;
}

.secret-field .el-input {
  flex: 1;
}

.secret-clear-btn {
  flex-shrink: 0;
}

/* Info icon next to form labels; hover/focus reveals the tooltip */
.help-icon {
  margin-left: var(--space-1);
  font-size: 14px;
  vertical-align: text-bottom;
  color: var(--text-secondary);
  cursor: help;
}

.help-icon:focus-visible {
  outline: 2px solid var(--accent-primary);
  outline-offset: 1px;
  border-radius: 50%;
}
</style>
