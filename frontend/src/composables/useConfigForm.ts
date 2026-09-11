import { ref, reactive, computed, watch, nextTick } from 'vue';
import { ElNotification, type FormRules } from 'element-plus';
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

/** Debounce window (ms) between the last edit and the auto-save PUT. */
const AUTO_SAVE_DEBOUNCE_MS = 500;

/**
 * Composable for the main Configuration form (Git platforms / Rules / Advanced).
 *
 * Owns the editable `config` model, dirty tracking against the last persisted
 * snapshot, debounced auto-save (every user edit saves itself — there is no
 * edit mode), validation rules, and the excluded-pattern tag input. LLM
 * provider management lives on the /llm page (unified provider cards with
 * immediate per-card persistence — see `useProviderCards`); the `llm` section
 * is loaded here only so the full config model stays complete, and the
 * auto-save payload drops it.
 *
 * @param cfg - The shared `useConfig()` instance used by the page.
 */
export function useConfigForm(cfg: ReturnType<typeof useConfig>) {
  const { t } = useI18n();

  /** Latest auto-save phase, surfaced as a header status indicator. */
  const saveStatus = ref<'idle' | 'saving' | 'saved' | 'error'>('idle');

  const config = reactive<AppConfig>(defaultConfig);
  /** Snapshot of the last persisted state; `null` before the first load. */
  const originalConfig = ref<AppConfig | null>(null);

  // --- Auto-save internals ---
  let saveTimer: ReturnType<typeof setTimeout> | null = null;
  /** Monotonic counter so a stale in-flight save can't clobber a newer one's status. */
  let saveSeq = 0;
  /** True between auto-save start and settle; blocks fetch-apply clobbering. */
  let saveInFlight = false;

  // --- Tag input state ---
  const patternInputVisible = ref(false);
  const patternInputValue = ref('');
  const patternInputRef = ref<any>();

  /** Function ref for the pattern input (string refs aren't visible to TS). */
  function setPatternInputRef(el: any) {
    patternInputRef.value = el;
  }

  // --- Computed ---

  /** True when the form differs from the last persisted snapshot. */
  const configDirty = computed(() => {
    if (!originalConfig.value) return false;
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

  /* Every user edit schedules a debounced auto-save. The dirty check at fire
   * time is the load guard: population from the initial fetch (and any
   * snapshot bookkeeping after a save) leaves the form equal to
   * `originalConfig`, so no PUT is issued for non-user changes. A failed save
   * keeps the form dirty, so the next user edit naturally retries. */
  watch(
    config,
    () => {
      if (!originalConfig.value) return;
      if (saveTimer) clearTimeout(saveTimer);
      saveTimer = setTimeout(autoSave, AUTO_SAVE_DEBOUNCE_MS);
    },
    { deep: true }
  );

  /** Build the PUT payload exactly like the old manual save: full copy minus `llm`. */
  function buildPayload(): Partial<AppConfig> {
    // LLM settings are managed on the LLM page (/llm): omit the `llm` key so
    // an auto-save never touches the stored LLM section (the backend
    // deep-merges the payload over the stored config; omitted sections are
    // preserved).
    const payload: Partial<AppConfig> = JSON.parse(JSON.stringify(config));
    delete payload.llm;
    return payload;
  }

  async function autoSave() {
    saveTimer = null;
    if (!configDirty.value) return;
    const seq = ++saveSeq;
    saveInFlight = true;
    saveStatus.value = 'saving';
    try {
      await cfg.save(buildPayload());
      if (seq !== saveSeq) return;
      // Re-snapshot the now-persisted state. The deep watcher fires on nothing
      // here (the form model is untouched), so this can't re-trigger a save.
      originalConfig.value = JSON.parse(JSON.stringify(config));
      saveStatus.value = 'saved';
    } catch {
      if (seq !== saveSeq) return;
      // Edits stay in the form and the form stays dirty: the next user edit
      // re-arms the debounce and retries the save.
      saveStatus.value = 'error';
      ElNotification({
        title: t('common.error'),
        message: t('config.saveFailed'),
        type: 'error',
        duration: 5000,
      });
    } finally {
      saveInFlight = false;
    }
  }

  // --- Methods ---
  /** Discard the transient pattern input row without committing it. */
  function discardPatternInput() {
    patternInputVisible.value = false;
    patternInputValue.value = '';
  }

  /** Apply freshly fetched config to the form and re-snapshot it. */
  function applyConfig(src: AppConfig) {
    Object.assign(config, src);
    backfillRequiredExperts();
    originalConfig.value = JSON.parse(JSON.stringify(config));
  }

  /**
   * Fetch the config from the server and apply it to the form.
   * While local edits are pending (dirty or an auto-save is in flight) the
   * fetched state is left in the `useConfig` cache but NOT written over the
   * form, so a background refresh can never clobber in-progress edits.
   */
  async function loadConfig() {
    await cfg.fetch();
    if (!cfg.config.value) return;
    if (configDirty.value || saveInFlight) return;
    applyConfig(cfg.config.value);
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

  /** Commit the transient row: push the pattern (auto-save picks it up). */
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
    saveStatus,
    rules,
    patternInputVisible,
    patternInputValue,
    setPatternInputRef,
    showPatternInput,
    addPattern,
    discardPatternInput,
    removePattern,
    loadConfig,
    refreshConfig,
  };
}
