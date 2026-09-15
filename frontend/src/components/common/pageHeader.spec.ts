import { describe, expect, it } from 'vitest';
import { createSSRApp, h } from 'vue';
import { renderToString } from '@vue/server-renderer';
import PageHeader from './PageHeader.vue';
// The component's own stylesheet, as text: the type scale and spacing are
// CSS the design language fixes, and this project has no DOM to measure.
import source from './PageHeader.vue?raw';

/**
 * PageHeader contract (RENG-76 R0.2).
 *
 * The component is rendered for real (SSR, no DOM needed — this project has
 * no jsdom/happy-dom test environment) so the heading level, the subtitle and
 * the actions slot are checked as they come out of the renderer; the type
 * scale and spacing are the component's own CSS contract and are pinned
 * against the source below.
 */

async function render(node: ReturnType<typeof h>): Promise<string> {
  return renderToString(createSSRApp({ render: () => node }));
}

describe('PageHeader', () => {
  it('renders the title as an H2 and the subtitle below it', async () => {
    const html = await render(h(PageHeader, { title: 'Review History', subtitle: 'All reviews' }));

    expect(html).toContain('<h2');
    expect(html).toContain('Review History');
    expect(html).toContain('All reviews');
    expect(html).not.toContain('<h1');
  });

  it('omits the subtitle element when there is none', async () => {
    const html = await render(h(PageHeader, { title: 'Review History' }));
    expect(html).not.toContain('page-header__subtitle');
  });

  it('renders the actions slot on the right', async () => {
    const html = await render(
      h(PageHeader, { title: 'LLM Status' }, { actions: () => h('button', { id: 'add' }, 'Add Provider') }),
    );

    expect(html).toContain('page-header__right');
    expect(html).toContain('Add Provider');
  });

  it('keeps the title at 20px inside a 24px-spaced header row', () => {
    expect(source).toMatch(/\.page-header\s*\{[^}]*margin-bottom:\s*24px/);
    expect(source).toMatch(/\.page-header__title\s*\{[^}]*font-size:\s*20px/);
    expect(source).toMatch(/\.page-header__title\s*\{[^}]*font-weight:\s*600/);
    expect(source).toMatch(/\.page-header__subtitle\s*\{[^}]*font-size:\s*13px/);
  });

  it('does not bring back the unscoped `.page-title` class', () => {
    expect(source).not.toMatch(/\.page-title\s*\{/);
  });
});
