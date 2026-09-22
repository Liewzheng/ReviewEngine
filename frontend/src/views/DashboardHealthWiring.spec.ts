import { describe, expect, it } from 'vitest';
import dashboard from './Dashboard.vue?raw';
import ellipsis from '../components/common/EllipsisText.vue?raw';

/**
 * RENG-103 — the System Health rows truncate on one line and reveal the full
 * text on hover.
 *
 * The health rows used to wrap: long provider names (`xiaomi mimo-v2.5`)
 * broke across lines, the 「错误」 badge stacked into 错/误, and the error
 * detail took 2–3 lines. The truncation lives inside a small shared
 * component — `EllipsisText` renders `el-tooltip` around its own span,
 * measures `scrollWidth > clientWidth` on `mouseenter`, and keeps the
 * tooltip disabled while the text fits — while the Dashboard supplies the
 * layout mechanics that let the spans actually shrink (`min-width: 0` on
 * every flex level) and the badge rules that keep it unwrappable.
 *
 * Pin it as source, like the RENG-84 and RENG-96 wiring specs: the failure
 * modes here are CSS and binding mistakes, and this project has no DOM to
 * measure them in.
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

describe('RENG-103 — the health rows are wired for truncation', () => {
  it('wraps the service name and the status message in the ellipsis component', () => {
    // Both sections — 集成状态 and LLM 提供商 — share the same row markup, so
    // the service-name wiring must appear twice and the message once per
    // section's shape (an i18n'd helper for integrations, a raw `message` for
    // LLM providers).
    expect(dashboard.match(/<EllipsisText :text="item\.service" \/>/g)).toHaveLength(2);
    expect(dashboard).toContain('<EllipsisText :text="integrationMessage(item) ?? \'\'" />');
    expect(dashboard).toContain('<EllipsisText :text="item.message ?? \'\'" />');
    // The timestamp keeps the same treatment, so a long relative date cannot
    // push the row wide either.
    expect(dashboard).toContain(":text=\"$t('common.lastTest', { date: integrationCheckedAt(item) })\"");
    expect(dashboard).toContain("import EllipsisText from '../components/common/EllipsisText.vue'");
  });

  it('truncates inside the shared component — one line, ellipsis, measured, disabled while it fits', () => {
    expect(rule(ellipsis, '.ellipsis-text')).toContain('text-overflow: ellipsis');
    expect(rule(ellipsis, '.ellipsis-text')).toContain('white-space: nowrap');
    expect(rule(ellipsis, '.ellipsis-text')).toContain('overflow: hidden');
    // The bubble only appears when the text really overflowed (RENG-103 口径 4:
    // a redundant tooltip on short text is noise).
    expect(ellipsis).toContain('scrollWidth > clientWidth');
    expect(ellipsis).toContain(':disabled="!truncated"');
    expect(ellipsis).toContain(':show-after="300"');
    expect(ellipsis).toContain('@mouseenter="updateTruncated"');
  });

  it('lets every flex level shrink so the ellipsis has room to engage', () => {
    expect(rule(dashboard, '.health-row-left')).toContain('min-width: 0');
    expect(rule(dashboard, '.health-row-right')).toContain('min-width: 0');
    expect(rule(dashboard, '.health-service')).toContain('min-width: 0');
  });

  it('keeps the status badge unwrappable and the timestamp fully visible', () => {
    // The 「错误」 stack came from the badge being squeezed; the badge must
    // neither shrink (it is StatusBadge's root, so the health-row scoped rules
    // reach it via :deep) nor let its text wrap.
    expect(rule(dashboard, '.health-row-right :deep(.status-badge)')).toContain('flex: 0 0 auto');
    expect(rule(dashboard, '.health-row-right :deep(.status-text)')).toContain('white-space: nowrap');
    // The 上次测试 line keeps its natural width and never shrinks; the message
    // span (default shrink) absorbs the squeeze instead.
    expect(rule(dashboard, '.health-latency-ts')).toContain('flex: 0 0 auto');
  });
});