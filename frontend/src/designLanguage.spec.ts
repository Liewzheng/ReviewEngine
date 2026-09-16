import { describe, expect, it } from 'vitest';
// Stylesheet sources, imported as text — see the note on this file above.
import styleCss from './style.css?raw';
import { CHART_PALETTE_FALLBACKS } from './chartPalette';
import pageHeaderSource from './components/common/PageHeader.vue?raw';
import providerCardSource from './components/Config/ProviderCardCompact.vue?raw';
import kpiCardSource from './components/Dashboard/KpiCard.vue?raw';
import statsCardSource from './components/QueueMonitor/StatsCard.vue?raw';
import expertCardSource from './components/ExpertsManagement/ExpertCard.vue?raw';
import llmStatusSource from './views/LlmStatus.vue?raw';
import expertsPageSource from './views/ExpertsManagement.vue?raw';
import dashboardSource from './views/Dashboard.vue?raw';
import historySource from './views/ReviewHistory.vue?raw';

/**
 * RENG-76 P0 design-language contract.
 *
 * These rules live in CSS — a root-cause font inheritance, a card's fixed box,
 * a table row's height — and this project has no DOM test environment to
 * measure them in (no jsdom/happy-dom). They are therefore pinned against the
 * stylesheet source: each assertion names the rule the design language
 * requires, so deleting or weakening it fails here instead of silently
 * shipping.
 *
 * The *behaviour* around these rules (which state a card is in, what the
 * footer says, the reorder payload) is covered by the render and unit suites
 * next to the components.
 */

const sources: Record<string, string> = {
  'src/style.css': styleCss,
  'src/components/common/PageHeader.vue': pageHeaderSource,
  'src/components/Config/ProviderCardCompact.vue': providerCardSource,
  'src/components/Dashboard/KpiCard.vue': kpiCardSource,
  'src/components/QueueMonitor/StatsCard.vue': statsCardSource,
  'src/components/ExpertsManagement/ExpertCard.vue': expertCardSource,
  'src/views/LlmStatus.vue': llmStatusSource,
  'src/views/ExpertsManagement.vue': expertsPageSource,
  'src/views/Dashboard.vue': dashboardSource,
  'src/views/ReviewHistory.vue': historySource,
};

const read = (path: string): string => sources[path] ?? '';

/** Collapse whitespace so a rule can be asserted regardless of formatting. */
const flat = (css: string) => css.replace(/\s+/g, ' ');

/** Concatenated bodies of every rule for `selector` (there may be several,
 *  including ones nested in `@media` blocks). */
function rule(css: string, selector: string): string {
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const source = flat(css);
  const pattern = new RegExp(`(?<![\\w.-])${escaped} \\{([^}]*)\\}`, 'g');
  const bodies: string[] = [];
  for (const match of source.matchAll(pattern)) bodies.push(match[1]);
  expect(bodies.length, `rule ${selector} not found`).toBeGreaterThan(0);
  return bodies.join(' ');
}

describe('R0.1 — controls inherit the surrounding type', () => {
  it('fixes font inheritance at the root, not on one button style', () => {
    expect(flat(read('src/style.css'))).toContain('button, input, select, textarea { font: inherit; }');
  });
});

describe('R0.3 — every card is the same height', () => {
  it('gives the provider card a fixed content box with a reserved footer slot', () => {
    const css = read('src/components/Config/ProviderCardCompact.vue');
    expect(rule(css, '.provider-card')).toContain('min-height: 183px');
    expect(rule(css, '.provider-card')).toContain('flex-direction: column');
    expect(rule(css, '.provider-card__header')).toContain('height: 36px');
    const footer = rule(css, '.provider-card__footer');
    expect(footer).toContain('height: 32px');
    // The reserved slot is what keeps footers aligned across the grid.
    expect(footer).toContain('margin-top: auto');
  });

  it('stretches the provider grid rows', () => {
    expect(rule(read('src/views/LlmStatus.vue'), '.provider-grid')).toContain('align-items: stretch');
  });

  it('equalises the expert card rows', () => {
    expect(rule(read('src/views/ExpertsManagement.vue'), '.experts-grid')).toContain('grid-auto-rows: 1fr');
    const card = rule(read('src/components/ExpertsManagement/ExpertCard.vue'), '.expert-card');
    expect(card).toContain('height: 100%');
    expect(card).toContain('flex-direction: column');
    expect(rule(read('src/components/ExpertsManagement/ExpertCard.vue'), '.card-actions')).toContain(
      'margin-top: auto',
    );
  });

  it('pins the dashboard recent-review rows to one height', () => {
    const css = read('src/views/Dashboard.vue');
    expect(rule(css, ':deep(.el-table__row)')).toContain('height: 48px');
    expect(rule(css, '.mr-title-cell')).toContain('align-items: center');
  });
});

describe('R0.4 — KPI numbers are restrained', () => {
  it('sets the KPI value scale and tabular figures', () => {
    const value = rule(read('src/components/Dashboard/KpiCard.vue'), '.kpi-value');
    expect(value).toContain('font-size: 28px');
    expect(value).toContain('font-weight: 600');
    expect(value).toContain('font-variant-numeric: tabular-nums');
  });

  it('renders an empty KPI in the tertiary colour', () => {
    expect(rule(read('src/components/Dashboard/KpiCard.vue'), '.kpi-value.is-empty')).toContain(
      'color: var(--text-tertiary)',
    );
    expect(rule(read('src/views/LlmStatus.vue'), '.stat-value.is-empty')).toContain(
      'color: var(--text-tertiary)',
    );
  });

  it('keeps the KPI subtitle at 12px, muted and single-line', () => {
    const subtitle = rule(read('src/components/Dashboard/KpiCard.vue'), '.kpi-trend');
    expect(subtitle).toContain('font-size: 12px');
    expect(subtitle).toContain('color: var(--text-secondary)');
    expect(subtitle).toContain('text-overflow: ellipsis');
    expect(subtitle).toContain('white-space: nowrap');
  });

  it('gives the queue statistics the same value scale', () => {
    const value = rule(read('src/components/QueueMonitor/StatsCard.vue'), '.stats-value');
    expect(value).toContain('font-size: 28px');
    expect(value).toContain('font-weight: 600');
    expect(value).toContain('font-variant-numeric: tabular-nums');
  });
});

describe('R0.5 — the history table is a grid', () => {
  it('pins every row to 48px and centres its cells', () => {
    const css = read('src/views/ReviewHistory.vue');
    expect(rule(css, '.history-table :deep(.el-table__row)')).toContain('height: 48px');
    const cell = rule(css, '.history-table :deep(.el-table__cell)');
    expect(cell).toContain('height: 48px');
    expect(cell).toContain('vertical-align: middle');
  });

  it('keeps the branch as a single muted chip in the list view', () => {
    const chip = rule(read('src/views/ReviewHistory.vue'), '.branch-chip');
    expect(chip).toContain('color: var(--text-secondary)');
    expect(chip).toContain('white-space: nowrap');
    expect(read('src/views/ReviewHistory.vue')).not.toContain('branch-name');
  });
});

describe('RENG-75 — the card’s state visuals', () => {
  const css = read('src/components/Config/ProviderCardCompact.vue');

  it('marks error and degraded with a 4px left stripe', () => {
    expect(rule(css, '.provider-card--accent-error')).toContain('border-left: 4px solid var(--accent-error)');
    expect(rule(css, '.provider-card--accent-degraded')).toContain(
      'border-left: 4px solid var(--accent-warning)',
    );
  });

  it('dims a disabled card end to end', () => {
    expect(rule(css, '.provider-card--disabled')).toContain('opacity: 0.55');
    // The usage share is the one part that keeps a colour of its own on a
    // stopped card; RENG-77 dropped the avatar this rule used to dim.
    expect(rule(css, '.provider-card--disabled .provider-card__usage-fill')).toContain(
      'background: var(--offline)',
    );
  });

  it('draws the usage share as a 1px hairline', () => {
    expect(rule(css, '.provider-card__usage-bar')).toContain('height: var(--progress-h-hairline)');
  });

  it('uses a grab cursor, with the header itself as the drag handle', () => {
    expect(rule(css, '.provider-card__header')).toContain('cursor: grab');
    expect(css).not.toContain('provider-card__drag-handle');
    // RENG-77: the mock's top-right mark is decoration ON that handle — a
    // muted icon pushed to the far edge, never a control with its own hit box.
    expect(rule(css, '.provider-card__grip')).toContain('margin-left: auto');
    expect(rule(css, '.provider-card__grip')).toContain('color: var(--text-tertiary)');
  });

  it('tells the URL and the model apart in the mock’s two greys', () => {
    expect(rule(css, '.provider-card__row--url')).toContain('color: var(--text-secondary)');
    expect(rule(css, '.provider-card__row--model')).toContain('color: var(--text-primary)');
    expect(rule(css, '.provider-card__row--key')).toContain('color: var(--text-tertiary)');
  });

  it('moves the health to a dot beside the name, and only when it matters', () => {
    expect(rule(css, '.provider-card__health')).toContain('border-radius: 50%');
    expect(rule(css, '.provider-card__health--degraded')).toContain('background: var(--accent-warning)');
    expect(rule(css, '.provider-card__health--error')).toContain('background: var(--accent-error)');
    // The right-aligned pill is gone from the card entirely.
    expect(css).not.toContain('provider-card__status');
  });

  it('draws the usage share with no caption line under it', () => {
    expect(css).not.toContain('provider-card__usage-label');
  });
});

describe('design tokens the rules depend on', () => {
  it('defines the tertiary text colour and the accent aliases', () => {
    const tokens = flat(read('src/style.css'));
    expect(tokens).toContain('--text-tertiary:');
    expect(tokens).toContain('--accent-primary: var(--brand)');
    expect(tokens).toContain('--accent-error: var(--error)');
  });
});

/**
 * RENG-76 P1/P2: the token layer itself (colour, radius, spacing, modal
 * widths), the Element Plus bridge that carries it into EP's own components,
 * and the badge/progress treatments that name it.
 */
const appSources = import.meta.glob('./**/*.{vue,ts,css}', {
  query: '?raw',
  import: 'default',
  eager: true,
}) as Record<string, string>;

/** The two files whose job is to hold a literal: the token layer, and the one
 *  canvas palette JavaScript cannot read a `var()` out of. */
const TOKEN_LAYER = new Set(['./style.css', './chartPalette.ts']);

const allSources = (): [string, string][] =>
  Object.entries(appSources).filter(([path]) => !path.endsWith('.spec.ts'));

const presentationSources = (): [string, string][] =>
  allSources().filter(([path]) => !TOKEN_LAYER.has(path));

/**
 * Source of an app file by its path relative to `src/`. Asserted to exist so a
 * renamed file fails loudly here instead of silently asserting against `''`.
 */
function source(relative: string): string {
  const text = appSources[`./${relative}`];
  expect(text, `${relative} is missing from the scanned sources`).toBeTypeOf('string');
  return text;
}

/** The value of every `padding` / `margin` / `gap` declaration in a source. */
function spacingValues(source: string): string[] {
  const pattern =
    /(?:^|[\s;,])(?:padding|margin|gap)(?:-(?:top|right|bottom|left|inline|inline-start|inline-end|block|block-start|block-end))?\s*:\s*([^;{}\n]*)/g;
  return [...source.matchAll(pattern)].map((match) => match[1]);
}

/** The px lengths a declaration value is built from. */
function pxLengths(value: string): number[] {
  return [...value.matchAll(/(?<![\w.])(\d+(?:\.\d+)?)px/g)].map((match) => Number(match[1]));
}

const RADIUS_VALUE = /^(?:var\(--radius-(?:sm|md|lg|pill)\)|50%|100%|0|0px)$/;

describe('R1.1 — colour lives in the token layer', () => {
  it('leaves no colour literal in a component or stylesheet', () => {
    const offenders: string[] = [];
    for (const [path, source] of presentationSources()) {
      for (const match of source.matchAll(/#[0-9a-fA-F]{3,8}\b|rgba?\(|hsla?\(/g)) {
        offenders.push(`${path}: ${match[0]}`);
      }
    }
    expect(offenders).toEqual([]);
  });

  it('keeps the one JS palette identical to the stylesheet it mirrors', () => {
    const tokens = flat(read('src/style.css'));
    for (const [name, value] of Object.entries(CHART_PALETTE_FALLBACKS)) {
      expect(tokens, `${name} has drifted from style.css`).toContain(`${name}: ${value};`);
    }
  });
});

describe('R1.2 — every corner is one of four radii', () => {
  const css = read('src/style.css');

  it('declares the four radii', () => {
    const tokens = flat(css);
    expect(tokens).toContain('--radius-sm: 6px');
    expect(tokens).toContain('--radius-md: 8px');
    expect(tokens).toContain('--radius-lg: 12px');
    expect(tokens).toContain('--radius-pill: 999px');
  });

  it('draws every border-radius from a token (circles and 0 excepted)', () => {
    const offenders: string[] = [];
    for (const [path, source] of presentationSources()) {
      for (const decl of source.matchAll(/border-radius\s*:\s*([^;{}\n]+)/g)) {
        const value = decl[1].trim();
        for (const part of value.split(/\s+/)) {
          if (!RADIUS_VALUE.test(part)) offenders.push(`${path}: ${value}`);
        }
      }
    }
    expect(offenders).toEqual([]);
  });

  it('overrides the Element Plus defaults in style.css, not per component', () => {
    const tokens = flat(css);
    expect(tokens).toContain('--el-border-radius-base: var(--radius-sm)');
    expect(tokens).toContain('--el-border-radius-small: var(--radius-sm)');
    expect(rule(css, '.el-pagination')).toContain(
      '--el-pagination-border-radius: var(--radius-sm)',
    );
    expect(rule(css, '.el-dialog')).toContain('--el-dialog-border-radius: var(--radius-md)');
    expect(rule(css, '.el-message-box')).toContain(
      '--el-messagebox-border-radius: var(--radius-md)',
    );
  });
});

describe('R1.3 — the Element Plus bridge covers both themes', () => {
  const tokens = flat(read('src/style.css'));

  it('maps EP onto the app palette in light and dark alike', () => {
    expect(tokens).toContain("html[data-theme='light'], html.dark {");
    expect(tokens).toContain('--el-bg-color: var(--bg-surface)');
    expect(tokens).toContain('--el-border-color: var(--border-color)');
    expect(tokens).toContain('--el-color-primary: var(--accent-primary)');
    expect(tokens).not.toContain('html.dark {\n  --el-bg-color');
  });

  it('re-mixes EP’s lighter/darker ladder from the same accent', () => {
    expect(tokens).toContain(
      '--el-color-success-light-9: color-mix(in srgb, var(--accent-success) 10%, var(--bg-surface))',
    );
    expect(tokens).toContain(
      '--el-color-primary-dark-2: color-mix(in srgb, var(--accent-primary) 80%, var(--bg-primary))',
    );
  });
});

describe('R1.4 — spacing comes off one six-step scale', () => {
  const css = read('src/style.css');

  it('declares the six steps', () => {
    const tokens = flat(css);
    for (const [token, value] of Object.entries({
      '--space-1': '4px',
      '--space-2': '8px',
      '--space-3': '12px',
      '--space-4': '16px',
      '--space-5': '24px',
      '--space-6': '32px',
    })) {
      expect(tokens, `${token} is missing`).toContain(`${token}: ${value};`);
    }
  });

  it('leaves no bare scale step and no off-scale step in the app', () => {
    const scale = new Set([4, 8, 12, 16, 24, 32]);
    const odd = new Set([3, 5, 14, 18]);
    const offenders: string[] = [];
    for (const [path, source] of allSources()) {
      for (const value of spacingValues(source)) {
        for (const px of pxLengths(value)) {
          if (scale.has(px)) offenders.push(`${path}: bare ${px}px in "${value.trim()}"`);
          if (odd.has(px)) offenders.push(`${path}: off-scale ${px}px in "${value.trim()}"`);
        }
      }
    }
    expect(offenders).toEqual([]);
  });

  it('moves Element Plus’ own off-scale spacing onto the scale', () => {
    expect(rule(css, '.el-form-item')).toContain('margin-bottom: var(--space-4)');
    expect(rule(css, '.el-empty')).toContain('--el-empty-padding: var(--space-6) 0');
    expect(rule(css, '.el-tag')).toContain('padding: 0 var(--space-2)');
  });
});

describe('R1.5 — the expert cards are monochrome', () => {
  const card = read('src/components/ExpertsManagement/ExpertCard.vue');

  it('drops the nine-colour category palette', () => {
    expect(source('types/expert.ts')).not.toContain('categoryColorMap');
    expect(card).not.toContain('categoryColorMap');
    expect(card).not.toContain('grayscale');
    expect(card).toContain("props.expert.enabled ? 'var(--accent-primary)' : 'var(--text-secondary)'");
  });

  it('turns the category chip into a muted surface', () => {
    const tag = rule(card, '.category-tag');
    expect(tag).toContain('background: var(--bg-hover)');
    expect(tag).toContain('color: var(--text-secondary)');
  });

  it('keeps the enabled/disabled switch legible', () => {
    expect(card).toContain(":active-color=\"'var(--accent-success)'\"");
  });
});

describe('R2.1 — one modal surface', () => {
  const css = read('src/style.css');

  it('gives the dialog, the message box and the drawer the same surface', () => {
    const surface = rule(css, '.el-dialog, .el-message-box, .el-drawer');
    expect(surface).toContain('background-color: var(--bg-elevated)');
    expect(surface).toContain('border: 1px solid var(--border-subtle)');
    expect(surface).toContain('border-radius: var(--radius-md)');
  });

  it('puts all three bodies on the same 12px padding', () => {
    expect(rule(css, '.el-dialog')).toContain('--el-dialog-padding-primary: var(--space-3)');
    expect(rule(css, '.el-message-box')).toContain(
      '--el-messagebox-padding-primary: var(--space-3)',
    );
    expect(rule(css, '.el-drawer')).toContain('--el-drawer-padding-primary: var(--space-3)');
  });
});

describe('R2.2 — the modal width scale', () => {
  it('declares the four steps', () => {
    const tokens = flat(read('src/style.css'));
    expect(tokens).toContain('--modal-w-sm: 420px');
    expect(tokens).toContain('--modal-w-md: 520px');
    expect(tokens).toContain('--modal-w-lg: 640px');
    expect(tokens).toContain('--modal-w-xl: 760px');
  });

  it('sends every dialog and drawer through the scale', () => {
    const offenders: string[] = [];
    for (const [path, source] of allSources()) {
      for (const match of source.matchAll(/\b(?:width|size)="(\d+px)"/g)) {
        offenders.push(`${path}: ${match[0]}`);
      }
    }
    expect(offenders).toEqual([]);
    expect(source('App.vue')).toContain('width="var(--modal-w-sm)"');
    expect(source('views/ExpertsManagement.vue')).toContain('width="var(--modal-w-lg)"');
    expect(source('components/Upgrade/UpgradeDialog.vue')).toContain(
      'width="var(--modal-w-md)"',
    );
    expect(source('views/ReviewHistory.vue')).toContain('size="var(--modal-w-lg)"');
  });
});

describe('R2.3 — status colours are the app’s accents', () => {
  it('paints the review-history status dots from the accents', () => {
    const css = source('components/ReviewHistory/StatusBadge.vue');
    expect(rule(css, '.status-dot.success')).toContain('background: var(--accent-success)');
    expect(rule(css, '.status-dot.warning')).toContain('background: var(--accent-warning)');
    expect(rule(css, '.status-dot.danger')).toContain('background: var(--accent-error)');
    expect(rule(css, '.status-dot.info')).toContain('background: var(--text-tertiary)');
  });

  it('rings the dashboard health dot with the accent, not a literal', () => {
    const css = source('components/Dashboard/StatusBadge.vue');
    expect(rule(css, '.status-dot.status-success')).toContain(
      'box-shadow: 0 0 0 2px var(--accent-success-ring)',
    );
    expect(rule(css, '.status-dot.status-queued')).toContain(
      'box-shadow: 0 0 0 2px var(--text-secondary-ring)',
    );
  });

  it('no longer knows any of the stock Element Plus status colours', () => {
    for (const [path, source] of allSources()) {
      for (const stock of ['#67c23a', '#e6a23c', '#f56c6c', '#909399']) {
        expect(source, `${path} still carries ${stock}`).not.toContain(stock);
      }
    }
  });
});

describe('R2.4 — a progress bar is a restraint', () => {
  const css = read('src/style.css');

  it('sizes every bar from the progress token', () => {
    expect(flat(css)).toContain('--progress-h: 2px;');
    expect(rule(css, '.el-progress-bar__outer')).toContain('height: var(--progress-h) !important');
    expect(rule(css, '.el-progress-bar__outer')).toContain('background-color: var(--bg-hover)');
    expect(rule(css, '.el-progress-bar__inner')).toContain('border-radius: var(--radius-pill)');
    expect(rule(css, '.el-progress-bar__inner')).toContain('opacity: 0.55');
  });

  it('strengthens the fill when the bar is hovered', () => {
    expect(flat(css)).toContain('.el-progress:hover .el-progress-bar__inner { opacity: 1;');
  });

  it('keeps the RENG-75 usage share on its own hairline token', () => {
    const card = read('src/components/Config/ProviderCardCompact.vue');
    expect(rule(card, '.provider-card__usage-bar')).toContain('height: var(--progress-h-hairline)');
  });

  it('leaves no component sizing a bar inline', () => {
    for (const [path, source] of allSources()) {
      expect(source, `${path} still binds :stroke-width`).not.toContain(':stroke-width=');
    }
  });
});

/**
 * RENG-77 — the UAT deltas: the primary button that rendered as an empty
 * rectangle, the KPI that wrapped onto two lines, the score badge that was
 * trimmed, and the labels for the two contract fields the Rust side adds
 * (`disableThinking` on the config echo, `avgTtfbMs` on the provider list).
 * Every one of them is a stylesheet rule or a locale string, so they are
 * pinned here the same way as the rest of the design language.
 */
describe('RENG-77 — the UAT deltas', () => {
  it('keeps the accent fill off the plain, text and link primary buttons', () => {
    const css = read('src/style.css');
    const solid = rule(css, '.el-button--primary:not(.is-plain):not(.is-text):not(.is-link)');
    expect(solid).toContain('background-color: var(--brand)');
    expect(solid).toContain('border-color: var(--brand)');
    // The bare selectors are what painted the plain variant indigo on indigo —
    // its text colour is `--el-color-primary`, which R1.3 bridged to that fill.
    expect(flat(css)).not.toContain('.el-button--primary {');
    expect(flat(css)).not.toContain('.el-button--primary:hover {');
  });

  it('holds a KPI value to one line, whatever it holds', () => {
    const value = rule(read('src/views/LlmStatus.vue'), '.stat-value');
    expect(value).toContain('white-space: nowrap');
    expect(value).toContain('overflow: hidden');
    expect(value).toContain('text-overflow: ellipsis');
  });

  it('drops the redundant inner padding on the score column', () => {
    const css = read('src/views/ReviewHistory.vue');
    // 28px of content for a 23–38px badge is what trimmed `88` to `88 ..`.
    expect(rule(css, '.history-table :deep(.col-score .cell)')).toContain('padding: 0');
    expect(css).toContain('class-name="col-score"');
    expect(css).toContain('label-class-name="col-score"');
  });

  it('labels the two new provider fields in all six locales', () => {
    for (const locale of ['en', 'zh-CN', 'zh-TW', 'ja', 'ko', 'fr']) {
      const file = source(`i18n/locales/${locale}.ts`);
      for (const key of ['disableThinking', 'disableThinkingHint', 'avgTtfb', 'avgTtfbWindow']) {
        expect(file, `${locale} is missing ${key}`).toContain(`${key}:`);
      }
    }
  });
});

/**
 * RENG-78 — the communication-latency label. The number is the probe's own
 * round trip, but the user is reading a duration and not a mechanism: the
 * label says what was measured (`平均通信延迟` / "Avg comm. latency") and never
 * the word 探测 / "probe". Pinned here as a stylesheet-style source check on
 * every locale; the value-level assertion lives in
 * `providerCardState.spec.ts`, which resolves the key the card actually uses.
 */
describe('RENG-78 — the communication-latency label', () => {
  it('exists in all six locales, in both the card and the KPI block', () => {
    for (const locale of ['en', 'zh-CN', 'zh-TW', 'ja', 'ko', 'fr']) {
      const file = source(`i18n/locales/${locale}.ts`);
      const stats = file.split('metrics: {')[0];
      expect(stats, `${locale} is missing the KPI label`).toContain('avgCommLatency:');
      expect(file.split('metrics: {')[1], `${locale} is missing the card label`).toContain('avgCommLatency:');
    }
  });

  it('never spells the mechanism into the label', () => {
    for (const locale of ['en', 'zh-CN', 'zh-TW', 'ja', 'ko', 'fr']) {
      const file = source(`i18n/locales/${locale}.ts`);
      // Every `avgCommLatency` line is a user-visible string; none of them may
      // name the probe.
      const lines = file.split('\n').filter((line) => line.includes('avgCommLatency:'));
      expect(lines, `${locale} carries no avgCommLatency`).not.toHaveLength(0);
      for (const line of lines) {
        expect(line, `${locale}: ${line}`).not.toContain('探测');
        expect(line, `${locale}: ${line}`).not.toContain('探測');
        expect(line.toLowerCase(), `${locale}: ${line}`).not.toContain('probe');
      }
    }
  });
});
