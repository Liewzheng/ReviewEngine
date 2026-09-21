import { describe, expect, it } from 'vitest';
import { reviewBranchLabel } from './reviewBranchLabel';

/**
 * The user asked for the source branch in the history list's chip ("也要展示
 * 目前分支名称，如 feat/xxxx->main 这样"). These are real unit tests on the pure
 * formatter — not the source-text assertions the view specs are limited to —
 * because every branch of the fallback is a string decision, not a layout one.
 */
describe('reviewBranchLabel pairs the source with the target', () => {
  it('renders source → target when they differ', () => {
    expect(reviewBranchLabel('feat/xxxx', 'main')).toBe('feat/xxxx → main');
    expect(reviewBranchLabel('refactor/session-detail-split', 'develop')).toBe(
      'refactor/session-detail-split → develop'
    );
  });

  it('keeps branch names verbatim, slashes and all', () => {
    expect(reviewBranchLabel('release/2026-q3-candidate', 'hotfix/0.10.47')).toBe(
      'release/2026-q3-candidate → hotfix/0.10.47'
    );
  });
});

describe('reviewBranchLabel falls back instead of inventing a half-pair', () => {
  it('shows the target alone when there is no source branch', () => {
    // Local-path and static-diff reviews carry `SourceMeta::default()`.
    expect(reviewBranchLabel('', 'main')).toBe('main');
    expect(reviewBranchLabel(null, 'main')).toBe('main');
    expect(reviewBranchLabel(undefined, 'main')).toBe('main');
  });

  it('collapses an identical pair to one name', () => {
    expect(reviewBranchLabel('main', 'main')).toBe('main');
    expect(reviewBranchLabel(' develop ', 'develop')).toBe('develop');
  });

  it('shows the source alone when only the target is missing', () => {
    expect(reviewBranchLabel('feat/xxxx', '')).toBe('feat/xxxx');
    expect(reviewBranchLabel('feat/xxxx', null)).toBe('feat/xxxx');
  });

  it('returns nothing when neither branch is known, so the chip can be omitted', () => {
    expect(reviewBranchLabel('', '')).toBe('');
    expect(reviewBranchLabel(null, undefined)).toBe('');
    expect(reviewBranchLabel('   ', '  ')).toBe('');
  });
});
