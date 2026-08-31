import { ref, reactive, computed, watch, nextTick } from 'vue';
import { ElNotification, type FormInstance, type FormRules } from 'element-plus';
import { useI18n } from 'vue-i18n';
import { useConfig } from './useConfig';
import type { AppConfig } from '../types/config';

/** Blank form model; empty secret/URL fields mean "keep the stored value". */
const defaultConfig: AppConfig = {
  llm: {
    apiBaseUrl: 'https://api.openai.com/v1',
    openaiApiKey: '',
    defaultModel: '',
    maxTokens: 4096,
    temperature: 0.7,
    timeoutSeconds: 60,
    retryAttempts: 3,
    primaryProvider: '',
    providers: [],
  },
  rules: {
    minScore: 75,
    blockOnCritical: true,
    autoCommentOnPass: true,
    commentTemplate: '',
    excludedPatterns: [],
    requiredExperts: [],
    maxReviewDurationSeconds: 300,
  },
  advanced: {
    logLevel: 'info',
    logRetentionDays: 30,
    sseHeartbeatInterval: 15,
    maxConcurrentReviews: 5,
    requestTimeout: 120,
    enableMetrics: true,
    debugMode: false,
  },
  gitPlatforms: [],
};

// Backend's documented default trio; used when GET /config returns an empty
// `requiredExperts` list so the form never starts permanently invalid (the
// validation rule requires at least one expert). Prefers the currently-enabled
// experts from the loaded config when the backend provides them.
const DEFAULT_REQUIRED_EXPERTS = ['Security', 'Performance', 'Quality'];

/**
 * Composable for the main Configuration form (Git platforms / Rules / Advanced).
 *
 * Owns the editable `config` model, edit-mode snapshotting, dirty tracking,
 * validation rules, and the excluded-pattern tag input. LLM provider management lives on the /llm page (unified provider
 * cards with immediate per-card persistence — see `useProviderCards`); the
 * `llm` section is loaded here only so the full config model stays complete,
 * and Configuration.vue drops it from its save payload.
 *
 * @param cfg - The shared `useConfig()` instance used by the page.
 */
export function useConfigForm(cfg: ReturnType<typeof useConfig>) {
  const { t } = useI18n();

  /** Whether the page is in edit mode (form inputs enabled). */
  const isEditing = ref(false);
  /** Latest validation result of the main form. */
  const formValid = ref(true);
  /** Element Plus form instance for the main form. */
  const formRef = ref<FormInstance>();

  const config = reactive<AppConfig>(defaultConfig);
  const originalConfig = ref<AppConfig | null>(null);

  // --- Tag input state ---
  const patternInputVisible = ref(false);
  const patternInputValue = ref('');
  const patternInputRef = ref<any>();

  /** Function ref for the pattern input (string refs aren't visible to TS). */
  function setPatternInputRef(el: any) {
    patternInputRef.value = el;
  }

  // --- Computed ---

  /** True when the main form differs from the snapshot taken on edit entry. */
  const configDirty = computed(() => {
    if (!isEditing.value || !originalConfig.value) return false;
    return JSON.stringify(config) !== JSON.stringify(originalConfig.value);
  });

  // --- Validation ---
  const rules = computed<FormRules>(() => ({
    'rules.requiredExperts': [
      {
        validator: (_rule: any, value: any, callback: any) => {
          if (!value || value.length === 0) {
            callback(new Error(t('config.validation.expertRequired')));
          } else {
            callback();
          }
        },
        trigger: 'change',
      },
    ],
  }));

  // --- Watchers ---
  function backfillRequiredExperts() {
    if (config.rules.requiredExperts.length > 0) return;
    const enabled = (config.experts ?? []).filter((e) => e.enabled).map((e) => e.name);
    config.rules.requiredExperts = enabled.length > 0 ? enabled : [...DEFAULT_REQUIRED_EXPERTS];
  }

  watch(
    config,
    () => {
      if (isEditing.value && formRef.value) {
        formRef.value
          .validate((valid: boolean) => {
            formValid.value = valid;
          })
          .catch(() => {
            formValid.value = false;
          });
      }
    },
    { deep: true }
  );

  // --- Methods ---
  /** Discard the transient (unsaved) pattern input row, if one is open. */
  function resetPatternInput() {
    patternInputVisible.value = false;
    patternInputValue.value = '';
  }

  /** Enter edit mode, snapshotting the current config for dirty tracking. */
  function enterEditMode() {
    originalConfig.value = JSON.parse(JSON.stringify(config));
    // Start clean: no half-typed pattern row left over from a previous edit.
    resetPatternInput();
    isEditing.value = true;
    formValid.value = true;
  }

  /**
   * Restore the edit-entry snapshot and leave edit mode.
   */
  function restoreSnapshot() {
    if (originalConfig.value) {
      Object.assign(config, originalConfig.value);
    }
    // Cancel drops any unsaved edits; the transient pattern input row is
    // local-only state and must disappear with them (the persisted
    // `excludedPatterns` list is untouched either way).
    resetPatternInput();
    isEditing.value = false;
    formValid.value = true;
  }

  /** Mark the current config as persisted after a successful save. */
  function commitSnapshot() {
    originalConfig.value = JSON.parse(JSON.stringify(config));
    resetPatternInput();
    isEditing.value = false;
  }

  /** Fetch the config from the server and apply it to the form. */
  async function loadConfig() {
    await cfg.fetch();
    if (cfg.config.value) {
      Object.assign(config, cfg.config.value);
      backfillRequiredExperts();
    }
  }

  /** Reload the config and notify the user. */
  async function refreshConfig() {
    await loadConfig();
    ElNotification({
      title: t('config.refreshedTitle'),
      message: t('config.refreshed'),
      type: 'info',
      duration: 2000,
    });
  }

  // --- Pattern tag input ---
  function showPatternInput() {
    patternInputVisible.value = true;
    nextTick(() => {
      patternInputRef.value?.focus();
    });
  }

  function addPattern() {
    const value = patternInputValue.value.trim();
    if (value && !config.rules.excludedPatterns.includes(value)) {
      config.rules.excludedPatterns.push(value);
    }
    patternInputVisible.value = false;
    patternInputValue.value = '';
  }

  function removePattern(index: number) {
    config.rules.excludedPatterns.splice(index, 1);
  }

  return {
    config,
    isEditing,
    formValid,
    formRef,
    configDirty,
    rules,
    patternInputVisible,
    patternInputValue,
    setPatternInputRef,
    showPatternInput,
    addPattern,
    removePattern,
    enterEditMode,
    restoreSnapshot,
    commitSnapshot,
    loadConfig,
    refreshConfig,
  };
}
