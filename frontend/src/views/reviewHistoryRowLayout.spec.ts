import { describe, expect, it } from 'vitest';
// Template and stylesheet sources, imported as text: this project has no DOM
// test environment (see the note at the top of `reviewHistoryList.spec.ts`).
// The layout these rules produce was measured in a headless Chrome against the
// production bundle — computed `white-space`, line-box count and geometry of
// `.duration-text` and `.branch-chip` at 1400px / 1025px / 390px, before and
// after. The assertions here are the regression guard for that measurement;
// the label logic itself is unit-tested in `reviewBranchLabel.spec.ts`.
import source from './ReviewHistory.vue?raw';
import { reviewBranchLabel } from '../composables/reviewBranchLabel';

// The stylesheet carries long explanatory comments, some of whose prose holds
// `;` and `property: value` pairs of its own — strip them before scraping
// declarations, or a comment's text swallows the declaration that follows it.
const flat = (css: string) =>
  css.replace(/\/\*[\s\S]*?\*\//g, ' ').replace(/\s+/g, ' ');

/** The `<style>` block at the end of the SFC. */
const styles = source.slice(source.lastIndexOf('<style'));

/** Concatenated bodies of every rule for `selector` (a rule must exist). */
function rule(css: string, selector: string): string {
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const pattern = new RegExp(`(?<![\\w.-])${escaped} \\{([^}]*)\\}`, 'g');
  const bodies = [...flat(css).matchAll(pattern)].map((match) => match[1]);
  expect(bodies.length, `rule ${selector} not found`).toBeGreaterThan(0);
  return bodies.join(' ');
}

/** The value of `property` in `selector`, or undefined. */
function value(css: string, selector: string, property: string): string | undefined {
  return [...rule(css, selector).matchAll(/([a-z-]+)\s*:\s*([^;]+)/g)]
    .map((match) => [match[1], match[2].trim()] as const)
    .find(([name]) => name === property)?.[1];
}

/** The opening tag that declares the column labelled `history.columns.<name>`. */
function columnTag(name: string): string {
  const tags = [...source.matchAll(/<el-table-column\b[^<>]*>/g)].map((match) => match[0]);
  const tag = tags.find((candidate) => candidate.includes(`history.columns.${name}`));
  expect(tag, `no column labelled history.columns.${name}`).toBeDefined();
  return tag as string;
}

/** The list row's branch chip, opening tag through closing tag. */
function chipElement(): string {
  const start = source.indexOf('class="branch-chip"');
  expect(start, 'no branch chip in the template').toBeGreaterThan(-1);
  const open = source.lastIndexOf('<div', start);
  const close = source.indexOf('</div>', start);
  return source.slice(open, close + '</div>'.length);
}

/**
 * The user reported, clicking through the 0.10.46 preview, that a long review
 * duration wrapped to a second line while a short one did not, and that the
 * history row's chip showed the merge target alone instead of the branch pair
 * the detail drawer already showed. Both fixes are CSS plus one template
 * binding, so both are pinned here as rules and bindings.
 */
describe('the duration cell keeps its value on one line', () => {
  it('lets the text refuse to wrap', () => {
    expect(value(styles, '.duration-text', 'white-space')).toBe('nowrap');
  });

  it('drops the cell padding layer that made a long value wrap', () => {
    // 100px column − 12px of td padding per side − 12px of `.cell` padding per
    // side left 52px for the text; `35m 29s` needs 54.6px at the 13px mono
    // face (7.8px per glyph), so it broke at the space into `35m` / `29s` and
    // the row grew from 48px to 49px. Dropping the inner layer — the same
    // remedy the score column already uses — takes the content box to 76px,
    // which holds the nine glyphs of `125m 59s`.
    expect(value(styles, '.history-table :deep(.col-duration .cell)', 'padding')).toBe('0');
    expect(columnTag('duration')).toContain('class-name="col-duration"');
    // Without the header class the label keeps its own padding and stops
    // lining up with the values underneath it.
    expect(columnTag('duration')).toContain('label-class-name="col-duration"');
  });

  it('keeps the column at the width M10 measured, so its neighbours do not move', () => {
    // The fix is padding, not width: at 100px the column already holds the
    // longest duration once the second padding layer is gone.
    expect(columnTag('duration')).toContain('width="100"');
  });
});

describe('the branch chip carries the source → target pair', () => {
  it('renders the shared label, not the target alone', () => {
    const chip = chipElement();
    expect(chip).toContain('reviewBranchLabel(row.branch, row.targetBranch)');
    expect(chip).not.toContain('{{ row.targetBranch }}');
  });

  it('keeps the label reachable when the chip ellipsizes', () => {
    // `title` is the hover surface; the element's text content already holds
    // the untruncated string for assistive tech, because `text-overflow`
    // truncates the painting, not the DOM.
    expect(chipElement()).toContain(':title="reviewBranchLabel(row.branch, row.targetBranch)"');
  });

  it('omits the chip entirely when neither branch is known', () => {
    // Local-path and static-diff reviews carry no branches; before this change
    // they rendered an empty 18px pill.
    expect(chipElement()).toContain('v-if="reviewBranchLabel(row.branch, row.targetBranch)"');
    expect(reviewBranchLabel('', '')).toBe('');
  });

  it('truncates a long pair instead of squeezing the MR title out', () => {
    // Pre-change, a 183px chip in the 390px layout left `.mr-title` 0px wide.
    expect(value(styles, '.branch-chip', 'max-width')).toBe('60%');
    expect(value(styles, '.branch-chip', 'overflow')).toBe('hidden');
    expect(value(styles, '.branch-chip', 'text-overflow')).toBe('ellipsis');
    expect(value(styles, '.branch-chip', 'white-space')).toBe('nowrap');
    // `flex: 0 0 auto` is what makes `max-width` the whole story: the chip
    // neither shrinks below its capped size nor grows into the title's share.
    expect(value(styles, '.branch-chip', 'flex')).toBe('0 0 auto');
    // The title keeps the rest and still ellipsizes rather than overflowing.
    expect(value(styles, '.mr-title', 'min-width')).toBe('0');
    expect(value(styles, '.mr-title', 'text-overflow')).toBe('ellipsis');
  });

  it('gives the chip cap a definite width to resolve against', () => {
    // `.title-text` used to shrink to fit its content, so a percentage
    // `max-width` inside it resolved against a width that depended on the very
    // content being capped. Filling the cell removes the circularity.
    expect(value(styles, '.title-text', 'flex')).toBe('1 1 auto');
    expect(value(styles, '.title-text', 'min-width')).toBe('0');
  });
});

describe('the list and the drawer read the branch the same way', () => {
  it('sends both surfaces through the shared label', () => {
    const drawer = source.slice(source.indexOf('history.drawer.branch'));
    expect(drawer.slice(0, 300)).toContain(
      'reviewBranchLabel(selectedReview.branch, selectedReview.targetBranch)'
    );
    // The drawer used to interpolate the pair by hand, which printed a
    // dangling `→ main` for a record without a source branch.
    expect(source).not.toContain('{{ selectedReview.branch }}');
    expect(source).not.toContain('&rarr;');
  });
});
