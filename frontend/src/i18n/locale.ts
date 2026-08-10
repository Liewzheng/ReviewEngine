// Supported-locale registry shared by the i18n instance (initial detection)
// and useLocale (runtime switching). Deliberately free of vue-i18n imports so
// both modules can depend on it without creating a circular import.
//
// Element Plus locale packages are imported from their `lang/*` subpaths one by
// one (on-demand) rather than from the `element-plus/es/locale` index, which
// would pull the entire locale catalog into the bundle.
import en from 'element-plus/es/locale/lang/en'
import zhCn from 'element-plus/es/locale/lang/zh-cn'
import zhTw from 'element-plus/es/locale/lang/zh-tw'
import ja from 'element-plus/es/locale/lang/ja'
import ko from 'element-plus/es/locale/lang/ko'
import fr from 'element-plus/es/locale/lang/fr'
import type { Language } from 'element-plus/es/locale'

/** localStorage key persisting the user's language choice. */
export const STORAGE_KEY = 'review_engine_locale'

export const SUPPORTED_LOCALES = ['en', 'zh-CN', 'zh-TW', 'ja', 'ko', 'fr'] as const
export type SupportedLocale = (typeof SUPPORTED_LOCALES)[number]

/** Element Plus locale package per app locale. */
export const ELEMENT_PLUS_LOCALES: Record<SupportedLocale, Language> = {
  en,
  'zh-CN': zhCn,
  'zh-TW': zhTw,
  ja,
  ko,
  fr,
}

export function isSupportedLocale(value: string): value is SupportedLocale {
  return (SUPPORTED_LOCALES as readonly string[]).includes(value)
}

/** Element Plus locale package for the given app locale. */
export function resolveElementPlusLocale(locale: SupportedLocale): Language {
  return ELEMENT_PLUS_LOCALES[locale]
}

/**
 * Initial locale resolution order:
 *   1. persisted `review_engine_locale` in localStorage
 *   2. navigator.language prefix match (zh/zh-CN → zh-CN, zh-TW/zh-HK → zh-TW,
 *      ja → ja, ko → ko, fr → fr)
 *   3. 'en' fallback
 */
export function detectInitialLocale(): SupportedLocale {
  if (typeof localStorage !== 'undefined') {
    const saved = localStorage.getItem(STORAGE_KEY)
    if (saved && isSupportedLocale(saved)) return saved
  }
  if (typeof navigator !== 'undefined') {
    const lang = navigator.language.toLowerCase()
    if (lang.startsWith('zh')) {
      if (lang.startsWith('zh-tw') || lang.startsWith('zh-hk')) return 'zh-TW'
      return 'zh-CN'
    }
    if (lang.startsWith('ja')) return 'ja'
    if (lang.startsWith('ko')) return 'ko'
    if (lang.startsWith('fr')) return 'fr'
  }
  return 'en'
}
