<script setup lang="ts">
import { ref, onMounted } from 'vue'
import { useI18n } from 'vue-i18n'
import { setSystemToken } from '../../services/system'
import { setApiToken } from '../../services/api'

const props = defineProps<{
  /**
   * First-run setup on a non-loopback bind (Docker / public deployments)
   * requires the one-time bootstrap key; loopback (local dev) needs none.
   */
  bootstrapKeyRequired: boolean
}>()

const emit = defineEmits<{ (e: 'done'): void }>()

const { t } = useI18n()

const token = ref('')
const bootstrapKey = ref('')
const saving = ref(false)
const errorMessage = ref<string | null>(null)

// el-input exposes a `focus()` method on its component instance.
const tokenInputRef = ref<{ focus: () => void } | null>(null)

onMounted(() => {
  // First-run screen: move focus to the token field so keyboard users can
  // type immediately (nothing else is reachable behind the modal).
  tokenInputRef.value?.focus()
})

function clearError() {
  errorMessage.value = null
}

async function submit() {
  // Guard against double-submit while the PUT is in flight.
  if (saving.value) return

  const tokenValue = token.value.trim()
  if (!tokenValue) {
    errorMessage.value = t('bootstrap.tokenRequiredError')
    return
  }
  if (props.bootstrapKeyRequired && !bootstrapKey.value.trim()) {
    errorMessage.value = t('bootstrap.keyRequiredError')
    return
  }

  saving.value = true
  errorMessage.value = null
  try {
    await setSystemToken(
      tokenValue,
      props.bootstrapKeyRequired ? bootstrapKey.value.trim() : undefined
    )
    // The server has persisted the token (auth.toml) and hot-swapped it into
    // effect. Cache it in localStorage as the session credential so every
    // subsequent request authenticates immediately.
    setApiToken(tokenValue)
    emit('done')
  } catch (e) {
    const code = (e as { code?: string })?.code
    if (code === 'bootstrap_key_required') {
      errorMessage.value = t('bootstrap.keyInvalidError')
    } else {
      errorMessage.value = t('bootstrap.saveError')
    }
  } finally {
    saving.value = false
  }
}
</script>

<template>
  <!-- Full-screen first-run view; replaces the app layout until a token is set. -->
  <div
    class="bootstrap-screen"
    role="dialog"
    aria-modal="true"
    aria-labelledby="bootstrap-title"
  >
    <form class="bootstrap-card" @submit.prevent="submit">
      <div class="brand" aria-hidden="true">🔍</div>
      <h1 id="bootstrap-title" class="title">{{ $t('bootstrap.title') }}</h1>
      <p class="intro">{{ $t('bootstrap.intro') }}</p>

      <label class="field-label" for="bootstrap-token">{{ $t('bootstrap.tokenLabel') }}</label>
      <el-input
        id="bootstrap-token"
        ref="tokenInputRef"
        v-model="token"
        type="password"
        show-password
        :placeholder="$t('bootstrap.tokenPlaceholder')"
        :disabled="saving"
        autocomplete="new-password"
        @input="clearError"
      />

      <template v-if="bootstrapKeyRequired">
        <label class="field-label" for="bootstrap-key">{{ $t('bootstrap.keyLabel') }}</label>
        <el-input
          id="bootstrap-key"
          v-model="bootstrapKey"
          type="password"
          show-password
          :placeholder="$t('bootstrap.keyPlaceholder')"
          :disabled="saving"
          autocomplete="off"
          @input="clearError"
        />
        <p class="field-hint" id="bootstrap-key-hint">{{ $t('bootstrap.keyHint') }}</p>
      </template>

      <p v-if="errorMessage" class="error" role="alert">{{ errorMessage }}</p>

      <el-button
        type="primary"
        native-type="submit"
        class="submit"
        :loading="saving"
        :disabled="saving"
      >
        {{ saving ? $t('bootstrap.saving') : $t('bootstrap.save') }}
      </el-button>
    </form>
  </div>
</template>

<style scoped>
.bootstrap-screen {
  min-height: 100vh;
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 24px;
  background-color: var(--bg-primary);
  /* Soft entrance; disabled for reduced-motion users. */
  animation: fade-in 0.18s ease-out;
}

@media (prefers-reduced-motion: reduce) {
  .bootstrap-screen {
    animation: none;
  }
}

@keyframes fade-in {
  from {
    opacity: 0;
  }
  to {
    opacity: 1;
  }
}

.bootstrap-card {
  width: 100%;
  max-width: 420px;
  padding: 32px 28px;
  background-color: var(--bg-surface);
  border: 1px solid var(--border-color);
  border-radius: var(--radius-lg);
  box-shadow: var(--shadow-card, 0 8px 30px rgba(0, 0, 0, 0.18));
}

.brand {
  font-size: 32px;
  text-align: center;
  margin-bottom: 12px;
}

.title {
  font-size: 18px;
  font-weight: 600;
  color: var(--text-primary);
  text-align: center;
  margin: 0 0 8px;
}

.intro {
  font-size: 13px;
  line-height: 1.6;
  color: var(--text-secondary);
  margin: 0 0 20px;
}

.field-label {
  display: block;
  font-size: 13px;
  font-weight: 500;
  color: var(--text-primary);
  margin: 14px 0 6px;
}

.field-hint {
  font-size: 12px;
  line-height: 1.5;
  color: var(--text-secondary);
  margin: 6px 0 0;
}

.error {
  font-size: 13px;
  color: var(--error);
  margin: 12px 0 0;
}

.submit {
  width: 100%;
  margin-top: 20px;
}
</style>
