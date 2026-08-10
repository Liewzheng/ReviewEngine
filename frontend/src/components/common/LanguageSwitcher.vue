<script setup lang="ts">
import { useI18n } from 'vue-i18n'
import { useLocale } from '../../composables/useLocale'
import { isSupportedLocale } from '../../i18n/locale'
import type { SupportedLocale } from '../../i18n/locale'

const { t } = useI18n()
const { locale, setLocale } = useLocale()

// Native names — intentionally locale-independent, so the menu reads the same
// in every language.
const options: { value: SupportedLocale; label: string }[] = [
  { value: 'en', label: 'English' },
  { value: 'zh-CN', label: '简体中文' },
  { value: 'zh-TW', label: '繁體中文' },
  { value: 'ja', label: '日本語' },
  { value: 'ko', label: '한국어' },
  { value: 'fr', label: 'Français' },
]

const onCommand = (command: string | number | object) => {
  if (typeof command === 'string' && isSupportedLocale(command)) {
    setLocale(command)
  }
}
</script>

<template>
  <el-dropdown trigger="click" @command="onCommand">
    <button
      type="button"
      class="language-switcher-trigger"
      :aria-label="t('header.language')"
    >
      <span class="language-switcher-icon" aria-hidden="true"></span>
      <span class="language-switcher-code">{{ locale }}</span>
    </button>
    <template #dropdown>
      <el-dropdown-menu>
        <el-dropdown-item
          v-for="option in options"
          :key="option.value"
          :command="option.value"
          :disabled="option.value === locale"
        >
          {{ option.label }}
        </el-dropdown-item>
      </el-dropdown-menu>
    </template>
  </el-dropdown>
</template>

<style scoped>
.language-switcher-trigger {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  padding: 5px 8px;
  border: none;
  border-radius: var(--radius-sm);
  background: transparent;
  color: var(--text-secondary);
  font-size: 12px;
  line-height: 1;
  cursor: pointer;
}

.language-switcher-trigger:hover {
  background: var(--bg-hover);
  color: var(--text-primary);
}

/* The language icon is the asset SVG applied as a CSS mask, tinted with the
   trigger's text color (currentColor) — visible in both light and dark themes
   without a second asset. */
.language-switcher-icon {
  width: 16px;
  height: 16px;
  flex-shrink: 0;
  background-color: currentColor;
  -webkit-mask-image: url('../../assets/language-switch.svg');
  mask-image: url('../../assets/language-switch.svg');
  -webkit-mask-repeat: no-repeat;
  mask-repeat: no-repeat;
  -webkit-mask-size: contain;
  mask-size: contain;
  -webkit-mask-position: center;
  mask-position: center;
}

.language-switcher-code {
  font-family: var(--font-mono);
  letter-spacing: 0.02em;
}
</style>
