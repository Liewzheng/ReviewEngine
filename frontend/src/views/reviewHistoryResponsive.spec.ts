import { describe, expect, it } from 'vitest';
// Template and stylesheet sources as text: this project has no DOM test
// environment (see the note at the top of `reviewHistoryList.spec.ts`). The
// rendered result of these breakpoints was checked separately in a headless
// Chrome against the production bundle — computed `display` and geometry of
// every cell at 390px / 900px / 1400px, before and after.
import source from './ReviewHistory.vue?raw';
import {
  HISTORY_DETAILS_MIN_WIDTH,
  HISTORY_PROJECT_MIN_WIDTH,
  historyTableColumns,
} from '../composables/useHistoryTableColumns';

/**
 * The phone-width history table used to hide EVERY cell: the `<=768px` block
 * hid all cells with a `:not(...):not(...)` selector (specificity 0,5,0) and
 * then tried to re-show title/status/actions with `:first-child` /
 * `:nth-child(4)` / `.is-fixed-right` (0,4,0 and 0,3,0), so the hide rule won
 * and the table rendered as an empty box. Restoring the re-show rules is not
 * enough either: a hidden cell keeps its column in Element Plus's fixed table
 * layout (measured: a 1026px table inside a 302px viewport, the sticky action
 * column covering the status cell). The narrow layouts therefore drop columns
 * from the table itself — pinned here.
 */

const flat = (css: string) => css.replace(/\s+/g, ' ');

/** The `<style>` block at the end of the SFC. */
const styles = source.slice(source.lastIndexOf('<style'));

/** Body of `@media (max-width: <max>px)`, brace-matched. */
function mediaBlock(css: string, max: number): string {
  const start = flat(css).indexOf(`@media (max-width: ${max}px) {`);
  expect(start, `media block ${max}px not found`).toBeGreaterThan(-1);
  let depth = 0;
  for (let i = flat(css).indexOf('{', start); i < flat(css).length; i++) {
    if (flat(css)[i] === '{') depth++;
    else if (flat(css)[i] === '}') {
      depth--;
      if (depth === 0) return flat(css).slice(start, i + 1);
    }
  }
  throw new Error(`unbalanced braces in the ${max}px media block`);
}

/** Opening tags of the list's `<el-table-column>`s, in render order. */
function columnTags(): string[] {
  return [...source.matchAll(/<el-table-column\b[^<>]*>/g)].map((match) => match[0]);
}

/** The opening tag that declares the column labelled `history.columns.<name>`. */
function columnTag(name: string): string {
  const tag = columnTags().find((candidate) => candidate.includes(`history.columns.${name}`));
  expect(tag, `no column labelled history.columns.${name}`).toBeDefined();
  return tag as string;
}

describe('the history table switch breakpoints', () => {
  it('keeps every column from 1025px up', () => {
    expect(historyTableColumns(HISTORY_PROJECT_MIN_WIDTH)).toEqual({
      project: true,
      details: true,
    });
    expect(historyTableColumns(1400)).toEqual({ project: true, details: true });
  });

  it('drops the project column from tablets down and the rest from phones down', () => {
    expect(historyTableColumns(HISTORY_PROJECT_MIN_WIDTH - 1)).toEqual({
      project: false,
      details: true,
    });
    expect(historyTableColumns(900)).toEqual({ project: false, details: true });
    expect(historyTableColumns(HISTORY_DETAILS_MIN_WIDTH)).toEqual({
      project: false,
      details: true,
    });
    expect(historyTableColumns(HISTORY_DETAILS_MIN_WIDTH - 1)).toEqual({
      project: false,
      details: false,
    });
    expect(historyTableColumns(390)).toEqual({ project: false, details: false });
  });
});

describe('the phone-width column set is title + status + actions', () => {
  it('leaves the title and the status column always rendered', () => {
    expect(columnTag('mrTitle')).not.toContain('v-if');
    expect(columnTag('status')).not.toContain('v-if');
  });

  it('binds every other column to the breakpoint that drops it', () => {
    expect(columnTag('project')).toContain('v-if="columns.project"');
    for (const name of ['author', 'score', 'duration', 'created']) {
      expect(columnTag(name), `${name} is not dropped on phones`).toContain(
        'v-if="columns.details"'
      );
    }
  });

  it('keeps the action column rendered and un-fixed once it fits the row', () => {
    const actions = columnTags().at(-1) as string;
    expect(actions).not.toContain('v-if');
    // `fixed="right"` pins the cell over the row's other cells; with only three
    // columns left there is nothing to scroll under, so the sticky overlay
    // would only hide the status badge on the narrowest phones.
    expect(actions).toContain(`:fixed="columns.details ? 'right' : false"`);
  });

  it('lets the title column shrink to the phone viewport', () => {
    expect(columnTag('mrTitle')).toContain(':min-width="columns.details ? 200 : 88"');
  });
});

describe('the fixed column widths stay where M10 measured them', () => {
  /**
   * The rendered widths at a 1400px viewport, from the headless-Chrome pass
   * that produced the numbers in `reviewHistoryRowLayout.spec.ts`: the title
   * column was 292px and the table 1118px inside a content area with room to
   * spare, so no horizontal scroll. The fixed columns below sum to 826px, and
   * the title column's 200px minimum makes the table's floor 1026px — which is
   * exactly the width Element Plus lays out at 1025px and 1200px, where the
   * visible area is already narrower than the table.
   *
   * The title column is the flexible one, so any width added to a fixed column
   * comes straight out of it. Pinning the set means a width change has to come
   * with a re-measurement instead of quietly eating the MR titles.
   */
  const MEASURED_FIXED_WIDTHS: [string, number][] = [
    ['project', 160],
    ['author', 160],
    ['status', 108],
    ['score', 76],
    // The duration fix (see `reviewHistoryRowLayout.spec.ts`) is padding, not
    // width: at 100px its cell holds `125m 59s` once the second padding layer
    // is gone, so the column stays where it was.
    ['duration', 100],
    ['created', 150],
  ];

  it('keeps every fixed column at its measured width', () => {
    for (const [name, width] of MEASURED_FIXED_WIDTHS) {
      expect(columnTag(name), `${name} changed width`).toContain(`width="${width}"`);
    }
  });

  it('leaves the actions column at its measured width', () => {
    const actions = columnTags().at(-1) as string;
    expect(actions).toContain('width="72"');
  });
});

describe('no cell-level hiding is left behind', () => {
  it('hides no table cell at any breakpoint', () => {
    expect(flat(styles)).not.toMatch(/\.el-table__cell[^{}]*\{[^}]*display:\s*none/);
    for (const max of [1024, 768]) {
      expect(mediaBlock(styles, max), `${max}px block still styles table cells`).not.toContain(
        'el-table'
      );
    }
  });

  it('no longer references the Element UI class Element Plus does not emit', () => {
    // Element Plus 2 emits `el-table-fixed-column--right`; `.is-fixed-right`
    // matched nothing, so that re-show rule could never fire.
    expect(source).not.toContain('is-fixed-right');
    expect(source).not.toContain('el-table-column--selection');
  });
});
