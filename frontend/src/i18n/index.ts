// Global vue-i18n instance (Composition API mode). Initial locale is resolved
// from localStorage → navigator.language → 'en' before the app mounts, and the
// <html lang> attribute is reflected immediately so the document language
// matches the UI on first paint.
import { createI18n } from 'vue-i18n'
import { detectInitialLocale } from './locale'
import en from './locales/en'
import zhCN from './locales/zh-CN'
import zhTW from './locales/zh-TW'
import ja from './locales/ja'
import ko from './locales/ko'
import fr from './locales/fr'

const messages = {
  en,
  'zh-CN': zhCN,
  'zh-TW': zhTW,
  ja,
  ko,
  fr,
}

const initialLocale = detectInitialLocale()

if (typeof document !== 'undefined') {
  document.documentElement.lang = initialLocale
}

export const i18n = createI18n({
  legacy: false,
  locale: initialLocale,
  fallbackLocale: 'en',
  messages,
})
