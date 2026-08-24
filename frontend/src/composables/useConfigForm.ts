import { ref, reactive, computed, watch, nextTick, onUnmounted } from 'vue';
import { ElNotification, type FormInstance, type FormRules } from 'element-plus';
import { useI18n } from 'vue-i18n';
import { useConfig } from './useConfig';
import type { AppConfig } from '../types/config';

/** Blank form model; empty secret/URL fields mean "keep the stored value". */
const defaultConfig: AppConfig = {
  gitlab: {
    url: '',
    apiToken: '',
    webhookSecret: '',
    webhookSigningSecret: '',
    defaultProject: '',
    mrLabel: '',
    autoReview: false,
  },
  llm: {
    apiBaseUrl: 'https://api.openai.com/v1',
    openaiApiKey: '',
    defaultModel: '',
    maxTokens: 4096,
    temperature: 0.7,
    timeoutSeconds: 60,
    retryAttempts: 3,
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

/** Secrets that can be temporarily revealed in read-only mode. */
export type RevealableField = 'apiToken' | 'webhookSecret' | 'webhookSigningSecret';

/**
 * Composable for the main Configuration form (GitLab / LLM / Rules / Advanced).
 *
 * Owns the editable `config` model, edit-mode snapshotting, dirty tracking,
 * validation rules, secret reveal timers, the excluded-pattern tag input, and
 * the debounced model list fetch. Provider management lives in
 * `useProviders`; the page composes the two.
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

  // --- Reveal state for read-only mode ---
  const revealed = reactive({
    apiToken: false,
    webhookSecret: false,
    webhookSigningSecret: false,
  });
  const revealCountdown = reactive({
    apiToken: 0,
    webhookSecret: 0,
    webhookSigningSecret: 0,
  });
  const revealTimers = reactive<Record<string, number>>({});

  // --- Tag input state ---
  const patternInputVisible = ref(false);
  const patternInputValue = ref('');
  const patternInputRef = ref<any>();

  /** Function ref for the pattern input (string refs aren't visible to TS). */
  function setPatternInputRef(el: any) {
    patternInputRef.value = el;
  }

  // --- Model list fetch state ---
  const modelOptions = ref<string[]>([]);
  const modelFetchLoading = ref(false);
  const modelFetchError = ref<string | null>(null);
  const modelFetchTimer = ref<number | null>(null);

  // --- Computed ---
  const availableModels = computed(() => modelOptions.value);

  /** True when the main form differs from the snapshot taken on edit entry. */
  const configDirty = computed(() => {
    if (!isEditing.value || !originalConfig.value) return false;
    return JSON.stringify(config) !== JSON.stringify(originalConfig.value);
  });

  // --- Validation ---
  // URL fields are only validated when a value is present: GET /config does not
  // echo every value (e.g. the GitLab URL is never mapped by the backend), so an
  // empty field means "keep the stored value", never a validation error.
  function validateUrl(_rule: any, value: string, callback: Function) {
    if (!value || !value.trim()) {
      callback();
      return;
    }
    try {
      new URL(value);
      callback();
    } catch {
      callback(new Error(t('config.validation.invalidUrl')));
    }
  }

  const rules = computed<FormRules>(() => ({
    'gitlab.url': [
      // GitLab URL may not be echoed by GET /config (not yet configured, or the
      // backend does not map it); empty = "keep the stored value", never a
      // validation error.
      { validator: validateUrl, trigger: 'blur' },
    ],
    'gitlab.apiToken': [
      {
        validator: (_rule: any, value: string, callback: Function) => {
          // GET /config returns the `***` mask when a token is configured; that
          // sentinel means "keep the stored token" and must never be
          // length-flagged. An empty value means "clear the token" (explicit
          // intent), also valid. Only a genuinely new token is length-checked.
          if (!value || value === '***' || value.length >= 10) {
            callback();
            return;
          }
          callback(new Error(t('config.validation.tokenMinLength')));
        },
        trigger: 'blur',
      },
    ],
    'gitlab.defaultProject': [
      {
        // Free-text project path; empty means "not set", otherwise it must
        // look like `group/project` (namespace/path, no extra slashes).
        validator: (_rule: any, value: string, callback: Function) => {
          if (!value || !value.trim()) {
            callback();
            return;
          }
          if (/^[^\s/]+\/[^\s/]+$/.test(value.trim())) {
            callback();
          } else {
            callback(new Error(t('config.validation.invalidProjectPath')));
          }
        },
        trigger: 'blur',
      },
    ],
    'llm.apiBaseUrl': [{ validator: validateUrl, trigger: 'blur' }],
    // The LLM API key shares the same keep-existing semantics as the GitLab
    // token (never echoed by GET /config), so it has no required rule.
    'llm.openaiApiKey': [],
    'llm.defaultModel': [
      { required: true, message: t('config.validation.modelRequired'), trigger: 'change' },
    ],
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

  watch(
    () => [config.llm.apiBaseUrl, config.llm.openaiApiKey],
    () => {
      if (modelFetchTimer.value) {
        clearTimeout(modelFetchTimer.value);
      }
      modelFetchTimer.value = window.setTimeout(() => {
        loadModels();
      }, 500);
    }
  );

  // --- Methods ---
  /** Fetch the model list from the configured LLM endpoint (debounced caller). */
  async function loadModels() {
    const apiBase = config.llm.apiBaseUrl.trim();
    const apiKey = config.llm.openaiApiKey.trim();
    if (!apiBase || !apiKey) {
      modelOptions.value = [];
      modelFetchError.value = null;
      return;
    }
    try {
      new URL(apiBase);
    } catch {
      modelOptions.value = [];
      return;
    }
    modelFetchLoading.value = true;
    modelFetchError.value = null;
    try {
      const models = await cfg.fetchModels(apiBase, apiKey);
      if (cfg.modelsError.value) {
        modelFetchError.value = cfg.modelsError.value;
        modelOptions.value = [];
      } else {
        modelOptions.value = models;
        if (!models.includes(config.llm.defaultModel)) {
          config.llm.defaultModel = models[0] || '';
        }
      }
    } finally {
      modelFetchLoading.value = false;
    }
  }

  /** Enter edit mode, snapshotting the current config for dirty tracking. */
  function enterEditMode() {
    originalConfig.value = JSON.parse(JSON.stringify(config));
    isEditing.value = true;
    formValid.value = true;
  }

  /**
   * Restore the edit-entry snapshot and leave edit mode. Callers should also
   * reset provider state (`useProviders().resetProviders`) when cancelling.
   */
  function restoreSnapshot() {
    if (originalConfig.value) {
      Object.assign(config, originalConfig.value);
    }
    isEditing.value = false;
    formValid.value = true;
  }

  /** Mark the current config as persisted after a successful save. */
  function commitSnapshot() {
    originalConfig.value = JSON.parse(JSON.stringify(config));
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

  /** Test connectivity to the configured primary LLM provider. */
  async function testConnection() {
    await cfg.test({
      provider: 'openai',
      model: config.llm.defaultModel,
      apiKey: config.llm.openaiApiKey,
      apiBase: config.llm.apiBaseUrl,
    });
  }

  /**
   * Reveal a secret in read-only mode for 5 seconds, then auto-hide it.
   * Re-revealing resets the countdown.
   */
  function revealField(field: RevealableField) {
    revealed[field] = true;
    revealCountdown[field] = 5;
    if (revealTimers[field]) clearInterval(revealTimers[field]);
    revealTimers[field] = window.setInterval(() => {
      revealCountdown[field]--;
      if (revealCountdown[field] <= 0) {
        clearInterval(revealTimers[field]);
        revealed[field] = false;
        delete revealTimers[field];
      }
    }, 1000);
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

  onUnmounted(() => {
    Object.values(revealTimers).forEach(clearInterval);
  });

  return {
    config,
    isEditing,
    formValid,
    formRef,
    configDirty,
    rules,
    revealed,
    revealCountdown,
    revealField,
    availableModels,
    modelFetchLoading,
    modelFetchError,
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
    testConnection,
  };
}
