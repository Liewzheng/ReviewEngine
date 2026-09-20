import { describe, expect, it } from 'vitest';
// Template, catalog and type sources, imported as text. This project has no
// DOM test environment, and an Element Plus table registers its columns in
// `onMounted` — which SSR never runs, so a server-rendered history table comes
// out with an empty `<thead>`. The column set therefore cannot be asserted
// from a render; it is pinned against the template and the catalogs instead,
// the same way `designLanguage.spec.ts` pins CSS. The rendered column list of
// both the old and the new build was compared in a real Chrome before and
// after the change (header cells `[MR TITLE, PROJECT, PARTICIPANTS, STATUS,
// SCORE, DURATION, CREATED, actions]`, no console errors).
import source from './ReviewHistory.vue?raw';
import typeSource from '../types/history.ts?raw';
import reviewsServiceSource from '../services/reviews.ts?raw';

/**
 * The user asked twice ("历史记录里不要显示 LLM 这一列" / "这一列不用显示") for the
 * LLM column to leave the review-history LIST. The detail drawer's per-expert
 * LLM tag is a different surface and stays.
 */

const catalogs = import.meta.glob('../i18n/locales/*.ts', {
  query: '?raw',
  import: 'default',
  eager: true,
}) as Record<string, string>;

const locales = Object.entries(catalogs).map(([path, text]) => [
  path.slice(path.lastIndexOf('/') + 1, -'.ts'.length),
  text,
]) as [string, string][];

/** The column labels the list template asks for, in render order. */
function listColumnKeys(): string[] {
  return [...source.matchAll(/\$t\('(history\.columns\.[a-zA-Z]+)'\)/g)].map((match) => match[1]);
}

describe('the review-history list carries no LLM column', () => {
  it('renders every other column and not the removed one', () => {
    expect(listColumnKeys()).toEqual([
      'history.columns.mrTitle',
      'history.columns.project',
      'history.columns.author',
      'history.columns.status',
      'history.columns.score',
      'history.columns.duration',
      'history.columns.created',
    ]);
  });

  it('leaves no cell renderer or class behind for it', () => {
    expect(source).not.toContain('history.columns.llm');
    expect(source).not.toContain('llm-cell');
    expect(source).not.toContain('formatLlmSummary');
  });
});

describe('the detail drawer keeps its per-expert LLM tag', () => {
  it('still labels each expert report with its provider/model', () => {
    // The tag is the drawer's own surface: `expertLlmLabel(exp)` on the expert
    // result, styled by `.llm-tag`, with its tooltip copy.
    expect(source).toContain('expertLlmLabel(exp)');
    expect(source).toContain('history.llm.expertTooltip');
    expect(source).toMatch(/\.llm-tag\s*\{/);
    expect(source).toContain('history.llm.unknown');
  });
});

describe('the removed label leaves no key behind in any locale', () => {
  it('drops `columns.llm` from all six catalogs', () => {
    // Six spaces of indent = the `history.columns` block; `nav.llm` sits at
    // four and is the LLM Status page's name, not a table column. Any value is
    // caught, not just the `'LLM'` the column used to carry — a re-added key
    // with a translated label would otherwise slip through.
    const left = locales.filter(([, text]) => /^ {6}llm:/m.test(text));
    expect(left.map(([name]) => name)).toEqual([]);
  });

  it('keeps the drawer copy the tag still needs', () => {
    for (const [name, text] of locales) {
      expect(text, `${name} lost the expert-tooltip copy`).toContain('expertTooltip:');
      expect(text, `${name} lost the unknown-provider fallback`).toContain('unknown:');
    }
  });
});

describe('the export is untouched: it serialises the API items, not the column list', () => {
  it('hands the whole list item to the payload', () => {
    // `handleExport` walks `getReviews` pages and puts the items in the JSON
    // payload verbatim, so the field travels with them whether or not a column
    // displays it — and `llmSummary` is still what the list endpoint returns.
    expect(source).toContain('total: items.length');
    expect(source).toContain('items,');
    expect(typeSource).toContain('llmSummary?:');
    expect(reviewsServiceSource).toContain('llmSummary:');
  });
});
