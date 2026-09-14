import { ref } from 'vue';

/** localStorage key holding the chosen theme across reloads. */
const STORAGE_KEY = 'theme';

// Module-scope singleton: the sidebar toggle in App.vue owns the interaction,
// but anything that renders from the theme's CSS vars — currently the
// Dashboard chart, which resolves `--chart-*` to concrete canvas colors —
// needs to react to a switch, so the state lives at module level (like a
// lightweight store) rather than as a ref local to App.vue.

/** True for the dark theme (the app's default when nothing is stored). */
const isDark = ref(true);

/**
 * Apply the theme to <html>: the bespoke `data-theme` attribute drives the
 * app's own CSS vars (style.css), while the `dark` class activates the
 * official Element Plus dark palette (theme-chalk/dark/css-vars.css), which
 * themes every EP component — including poppers, drawers, and dialogs that
 * mount on <body> and never saw the bespoke vars.
 */
function applyTheme(dark: boolean) {
  document.documentElement.setAttribute('data-theme', dark ? 'dark' : 'light');
  document.documentElement.classList.toggle('dark', dark);
}

/** Switch to the given theme, apply it to <html>, and persist the choice. */
function setTheme(dark: boolean) {
  isDark.value = dark;
  applyTheme(dark);
  localStorage.setItem(STORAGE_KEY, dark ? 'dark' : 'light');
}

/** Flip light/dark — the sidebar toggle's action. */
function toggleTheme() {
  setTheme(!isDark.value);
}

/** Restore the persisted theme (dark when unset) and apply it. */
function initTheme() {
  const saved = localStorage.getItem(STORAGE_KEY);
  isDark.value = saved ? saved === 'dark' : true;
  applyTheme(isDark.value);
}

/**
 * The app's theme state, shared app-wide. `isDark` is reactive, so a consumer
 * whose output is painted from the theme vars (a canvas chart cannot follow a
 * CSS-variable change on its own) can `watch(isDark)` and re-apply its colors.
 */
export function useTheme() {
  return { isDark, setTheme, toggleTheme, initTheme };
}
