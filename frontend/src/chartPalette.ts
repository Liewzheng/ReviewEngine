/**
 * The trend chart's palette in JS form.
 *
 * `lightweight-charts` paints on canvas and parses a colour on a detached
 * scratch context, which cannot resolve a CSS `var(--…)` reference: an
 * unresolvable string is dropped silently and the chart falls back to
 * near-black. Dashboard.vue therefore resolves `--chart-*` through
 * `getComputedStyle` on every apply, and falls back to the values below when
 * the stylesheet has not been applied to the document yet.
 *
 * This is the one palette the app writes twice: it mirrors the `:root`
 * declarations in `style.css`, and `designLanguage.spec.ts` asserts the two
 * agree, so the copy cannot drift out of the design language unnoticed.
 */
export const CHART_PALETTE_FALLBACKS: Record<string, string> = {
  '--chart-grid': 'rgba(148, 163, 184, 0.28)',
  '--chart-text': '#cbd5e1',
  '--chart-line': '#818cf8',
  '--chart-bar': '#a78bfa',
  '--bg-primary': '#121314',
};

/** Last resort for a series colour when the palette above has no entry. */
export const CHART_SERIES_FALLBACK = CHART_PALETTE_FALLBACKS['--chart-bar'];
