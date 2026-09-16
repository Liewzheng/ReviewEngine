import { describe, expect, it } from 'vitest';
// The drawer's stylesheet, as text — see the note at the top of
// `designLanguage.spec.ts`: this project has no DOM to measure CSS in.
import source from './ReviewHistory.vue?raw';

/**
 * RENG-84 — the review-detail drawer's two visual contracts.
 *
 * The metadata grid used to be six cards of its own: a `--bg-surface` fill
 * inside a `--border-color` frame each. R2.1 lifted the drawer surface to
 * `--bg-elevated`, one step ABOVE `--bg-surface`, so the six tiles kept the
 * darker fill and their frames and read as six dirty boxes floating on a
 * lighter panel. The expert list had the mirror-image problem: Element Plus
 * paints the collapse header and wrap from `--el-collapse-*-bg-color`, which
 * the R1.3 bridge resolves to that same `--bg-surface`, so every row was a dark
 * block and nine of them stacked into one slab.
 *
 * Both are pinned here as stylesheet rules, because weakening one fails nowhere
 * else: the layout is CSS and there is no DOM to measure it in. The box
 * geometry these rules produce was measured in a real render of the production
 * bundle, dark and light, before and after — see `reports-reng84/`.
 */

const flat = (css: string) => css.replace(/\s+/g, ' ');

/** Concatenated bodies of every rule for `selector` (a rule must exist). */
function rule(css: string, selector: string): string {
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const pattern = new RegExp(`(?<![\\w.-])${escaped} \\{([^}]*)\\}`, 'g');
  const bodies = [...flat(css).matchAll(pattern)].map((match) => match[1]);
  expect(bodies.length, `rule ${selector} not found`).toBeGreaterThan(0);
  return bodies.join(' ');
}

/** Every declaration in a rule body, as `[property, value]`. */
function declarations(body: string): [string, string][] {
  return [...body.matchAll(/([a-z-]+)\s*:\s*([^;]+)/g)].map((match) => [match[1], match[2].trim()]);
}

/** The properties of a rule, in source order. */
function properties(css: string, selector: string): string[] {
  return declarations(rule(css, selector)).map(([property]) => property);
}

const PAINTED = /^(background|background-color|border|border-[a-z-]+|color|box-shadow|outline)$/;
const TOKENISED = /^(transparent|none|0|inherit|currentColor)$|var\(--/;

/** The declarations that DRAW a box — a fill or a line, never a corner. */
const FRAME_PROPERTIES = new Set([
  'background',
  'background-color',
  'border',
  'border-color',
  'border-style',
  'border-width',
  'border-top',
  'border-right',
  'border-bottom',
  'border-left',
  'box-shadow',
]);

/** Which of a rule's declarations draw a box, in source order. */
function frame(css: string, selector: string): string[] {
  return properties(css, selector).filter((property) => FRAME_PROPERTIES.has(property));
}

const METRICS = '.meta-grid';
const META_ITEM = '.meta-item';
const COLLAPSE = '.expert-collapse';
// EP's own selector for the row is `.el-collapse-item__header`; `:hover` sits
// inside the `:deep()`, as the stylesheet writes it.
const HEADER = '.expert-collapse :deep(.el-collapse-item__header)';
const HEADER_HOVER = `${HEADER.slice(0, -1)}:hover)`;
const LAST_HEADER =
  '.expert-collapse :deep(.el-collapse-item:last-child .el-collapse-item__header)';

describe('RENG-84 — the metadata grid is flat', () => {
  it('leaves each field unframed, unfilled and unpadded', () => {
    // The old tile, per field: --bg-surface fill, --border-color frame,
    // --radius-md corner and 8px/12px of its own padding.
    expect(frame(source, META_ITEM)).toEqual([]);
    expect(properties(source, META_ITEM)).not.toContain('padding');
    expect(properties(source, META_ITEM)).not.toContain('border-radius');
  });

  it('separates the fields with the column gap and the icon instead', () => {
    const grid = rule(source, METRICS);
    expect(grid).toContain('grid-template-columns: repeat(2, 1fr)');
    // The 24px column gap is what the six frames used to do — it is now the
    // only thing holding the two columns apart.
    expect(grid).toContain('column-gap: var(--space-5)');
    expect(grid).toContain('row-gap: var(--space-3)');
    const item = rule(source, META_ITEM);
    expect(item).toContain('align-items: flex-start');
    expect(item).toContain('gap: var(--space-2)');
  });

  it('keeps the icon quiet and on the label’s line', () => {
    const icon = rule(source, `${META_ITEM} .el-icon`);
    expect(icon).toContain('color: var(--text-tertiary)');
    expect(icon).toContain('flex-shrink: 0');
    // A marker beside the label, not a glyph of a tile.
    expect(icon).toContain('font-size: 14px');
    // The label stays the greyer of the two, the value the louder one.
    expect(rule(source, '.meta-label')).toContain('color: var(--text-secondary)');
    expect(rule(source, '.meta-value')).toContain('color: var(--text-primary)');
  });
});

describe('RENG-84 — the expert rows are a list on the drawer surface', () => {
  it('repaints Element Plus’s dark slab transparent and compacts the row', () => {
    const root = rule(source, COLLAPSE);
    expect(root).toContain('--el-collapse-header-bg-color: transparent');
    expect(root).toContain('--el-collapse-content-bg-color: transparent');
    // EP's row is 48px; the list is a list now.
    expect(root).toContain('--el-collapse-header-height: 44px');
  });

  it('keeps the rule that starts the list and drops the one on the divider', () => {
    // These land on `.el-collapse` itself — the element the class is on, which
    // is why no `:deep()` descendant selector could ever reach them.
    const root = rule(source, COLLAPSE);
    expect(root).toContain('border-top: 1px solid var(--border-color)');
    expect(root).toContain('border-bottom: none');
  });

  it('drops the hairline under the last row, which the section divider closes', () => {
    expect(rule(source, LAST_HEADER)).toContain('border-bottom: none');
  });

  it('keeps no per-row card frame', () => {
    // No rule paints the header at all: the only fill a row ever shows is the
    // hover tint below, and a future `…__header { background: … }` fails here.
    expect(flat(source)).not.toContain(`${HEADER} {`);
    expect(frame(source, `${HEADER_HOVER}`)).toEqual(['background-color']);
  });

  it('marks the row with a full-width tint, like a table row', () => {
    const hover = rule(source, `${HEADER_HOVER}`);
    expect(hover).toContain('background-color: var(--bg-hover)');
    // No corner and no lift: a rounded, shadowed row would be a card again.
    expect(properties(source, `${HEADER_HOVER}`)).not.toContain('border-radius');
  });

  it('keeps the chevron, quietly', () => {
    expect(rule(source, '.expert-collapse :deep(.el-collapse-item__arrow)')).toContain(
      'color: var(--text-tertiary)'
    );
  });

  it('replaces EP’s off-scale 25px of panel padding', () => {
    expect(rule(source, '.expert-collapse :deep(.el-collapse-item__content)')).toContain(
      'padding-bottom: var(--space-2)'
    );
  });

  it('tightens the air between the score tag and the chevron to the 44px row', () => {
    expect(rule(source, '.expert-title')).toContain('padding-right: var(--space-2)');
  });

  it('reserves the score column for the status badge, not for the score itself', () => {
    // `.el-tag:only-child` is also true of a row whose only tag is the score —
    // the usual row, since the badge is rendered for non-success statuses only.
    // Those rows sat 50px short of the score column they were meant to line up
    // with, so the score never reached the row's right edge.
    expect(flat(source)).not.toContain('.expert-meta > .el-tag:only-child {');
    expect(rule(source, '.expert-meta > .status-badge:only-child')).toContain('margin-right: 50px');
    // The two width floors the reservation is measured against.
    expect(rule(source, '.expert-meta > .el-tag:first-child')).toContain('min-width: 54px');
    expect(rule(source, '.expert-meta > .el-tag + .el-tag')).toContain('min-width: 42px');
  });
});

describe('RENG-84 — the new rules speak the token layer', () => {
  const changed = [
    METRICS,
    META_ITEM,
    `${META_ITEM} .el-icon`,
    '.meta-label',
    '.meta-value',
    COLLAPSE,
    LAST_HEADER,
    `${HEADER_HOVER}`,
    '.expert-collapse :deep(.el-collapse-item__arrow)',
    '.expert-collapse :deep(.el-collapse-item__content)',
    '.expert-title',
    '.expert-markdown',
  ];

  it('paints every one of them from a token, never a literal', () => {
    const offenders: string[] = [];
    for (const selector of changed) {
      for (const [property, value] of declarations(rule(source, selector))) {
        if (PAINTED.test(property) && !TOKENISED.test(value)) {
          offenders.push(`${selector} { ${property}: ${value} }`);
        }
      }
    }
    expect(offenders).toEqual([]);
  });

  it('takes every spacing step off the scale', () => {
    const offenders: string[] = [];
    for (const selector of changed) {
      for (const [property, value] of declarations(rule(source, selector))) {
        if (/^(padding|margin|gap)/.test(property) && !/^var\(--space-|^0( |$)/.test(value)) {
          offenders.push(`${selector} { ${property}: ${value} }`);
        }
      }
    }
    expect(offenders).toEqual([]);
  });

  it('leaves the report the one framed box in the list', () => {
    // One fill, one line. The hairline is load-bearing: in the light theme
    // `--bg-surface` and the drawer's `--bg-elevated` are both white, so the
    // colour step that marks the well in dark would vanish without it.
    expect(frame(source, '.expert-markdown')).toEqual(['background', 'border']);
    expect(rule(source, '.expert-markdown')).toContain('background: var(--bg-surface)');
    expect(rule(source, '.expert-markdown')).toContain('border: 1px solid var(--border-color)');
  });
});
