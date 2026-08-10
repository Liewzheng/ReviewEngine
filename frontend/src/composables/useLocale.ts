import { computed } from 'vue';
import { i18n } from '../i18n';
import {
  isSupportedLocale,
  resolveElementPlusLocale,
  STORAGE_KEY,
  SUPPORTED_LOCALES,
} from '../i18n/locale';
import type { SupportedLocale } from '../i18n/locale';

/**
 * App-locale helpers bound to the shared vue-i18n instance (the i18n singleton
 * owns the state, so this composable is stateless — no module-scope store):
 *  - `locale` — current app locale, always one of SUPPORTED_LOCALES
 *  - `elementLocale` — matching Element Plus locale package, feed this into
 *    <el-config-provider :locale="...">
 *  - `setLocale(locale)` — switch i18n locale (immediate, no reload), sync
 *    <html lang>, and persist to localStorage.review_engine_locale
 *  - `supportedLocales` — ordered list of selectable locales
 */
export function useLocale() {
  const locale = computed<SupportedLocale>(() => {
    const current = i18n.global.locale.value;
    return isSupportedLocale(current) ? current : 'en';
  });

  const elementLocale = computed(() => resolveElementPlusLocale(locale.value));

  function setLocale(next: SupportedLocale) {
    i18n.global.locale.value = next;
    if (typeof document !== 'undefined') {
      document.documentElement.lang = next;
    }
    try {
      localStorage.setItem(STORAGE_KEY, next);
    } catch {
      // Storage unavailable (private mode / quota) — locale still applies for
      // the session even though it won't persist across reloads.
    }
  }

  return { locale, elementLocale, setLocale, supportedLocales: SUPPORTED_LOCALES };
}
