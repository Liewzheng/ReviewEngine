import { describe, expect, it } from 'vitest';
// Stylesheet sources, imported as text — see the note on this file above.
import styleCss from './style.css?raw';
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
    expect(rule(css, '.provider-card--disabled .provider-card__monogram')).toContain(
      'background: var(--text-tertiary)',
    );
  });

  it('draws the usage share as a 1px hairline', () => {
    expect(rule(css, '.provider-card__usage-bar')).toContain('height: 1px');
  });

  it('uses a grab cursor and no drag handle element', () => {
    expect(rule(css, '.provider-card__header')).toContain('cursor: grab');
    expect(css).not.toContain('provider-card__drag-handle');
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
