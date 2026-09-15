import { describe, expect, it } from 'vitest';
import { createSSRApp, h } from 'vue';
import { renderToString } from '@vue/server-renderer';
import { createI18n } from 'vue-i18n';
import { ID_INJECTION_KEY } from 'element-plus';
import ProviderCardCompact from './ProviderCardCompact.vue';
import ProviderContextMenu from './ProviderContextMenu.vue';
import { MOCK_CARDS, MOCK_PROVIDERS } from '../../dev-mocks/llm-providers.mock';
import type { ProviderCardState } from '../../composables/llmPayload';
import type { CardHealth } from './providerCardState';

/**
 * Card and context-menu render contract (RENG-75).
 *
 * Rendered for real through the SSR renderer — the project has no DOM test
 * environment (no jsdom/happy-dom installed), and this is the closest thing
 * to "mount the component and look at it": the classes each state carries,
 * the rows that must always be present, and the words the footer slot and
 * the menu put on screen. Layout *measurement* (equal pixel heights) is
 * pinned separately in `designLanguage.spec.ts`, against the CSS that fixes
 * the card's box.
 */

const messages = {
  common: { edit: 'Edit', testConnection: 'Test Connection' },
  llm: {
    status: { healthy: 'Healthy', degraded: 'Degraded', error: 'Error', offline: 'Offline', disabled: 'Disabled' },
    metrics: { avgLatency: 'Avg Latency', requests: 'Usages', successRate: 'Success Rate' },
    card: { disabled: 'Disabled', probeFailed: 'Probe failed', testOk: 'Test OK', testFailed: 'Test failed' },
    usageShare: '{percent}% of usage (last {days} days)',
  },
  config: {
    providerCards: {
      chainPosition: 'Chain #{n}',
      duplicate: 'Duplicate Card',
      enable: 'Enable',
      disable: 'Disable',
      deleteTitle: 'Delete Provider',
    },
  },
};

const i18n = createI18n({ legacy: false, locale: 'en', messages: { en: messages } });

function card(over: Partial<ProviderCardState> = {}): ProviderCardState {
  return {
    provider: 'anthropic',
    apiKey: '***',
    apiBaseUrl: 'https://api.anthropic.com',
    defaultModel: 'claude-sonnet-4-5',
    maxTokens: 4096,
    temperature: 0.7,
    timeoutSeconds: 60,
    retryAttempts: 3,
    disabled: false,
    ...over,
  };
}

async function renderCard(props: {
  card: ProviderCardState;
  health?: CardHealth;
  probeMessage?: string | null;
  usageWindowDays?: number | null;
}): Promise<string> {
  const app = createSSRApp({
    render: () =>
      h(ProviderCardCompact, {
        index: 0,
        usageWindowDays: 7,
        ...props,
      }),
  }).use(i18n);
  // Element Plus warns without a deterministic id source during SSR.
  app.provide(ID_INJECTION_KEY, { prefix: 1, current: 0 });
  return renderToString(app);
}

describe('provider card layout', () => {
  it('always renders the same rows, whatever the state', async () => {
    // Five content rows plus the reserved footer — the layout must not
    // shrink for a card with less data, or the grid would misalign. The
    // usage row is the one part the spec hides entirely when there is no
    // share to show.
    for (const health of MOCK_PROVIDERS) {
      const html = await renderCard({ card: card({ provider: health.name }), health });
      for (const row of ['provider-card__header', 'provider-card__row', 'provider-card__stats', 'provider-card__footer']) {
        expect(html).toContain(row);
      }
      expect(html.match(/provider-card__row provider-card__row--key/g)).toHaveLength(1);
    }
  });

  it('renders the URL, the model and the masked key — never a key', async () => {
    const html = await renderCard({ card: card(), health: undefined });
    expect(html).toContain('https://api.anthropic.com');
    expect(html).toContain('claude-sonnet-4-5');
    expect(html).toContain('••••••');
    expect(html).toContain('AN');
  });

  it('shows the three statistics with their values', async () => {
    const html = await renderCard({
      card: card(),
      health: { name: 'anthropic', status: 'healthy', disabled: false, avgLatencyMs: 300, requestCount: 7, successRate: 1 },
    });
    expect(html).toContain('300ms');
    expect(html).toContain('>7<');
    expect(html).toContain('100.0%');
  });

  it('renders an unmeasured statistic as an em dash in the muted colour', async () => {
    const html = await renderCard({
      card: card(),
      health: { name: 'anthropic', status: 'error', disabled: false, avgLatencyMs: null, requestCount: 128, successRate: 0.821 },
    });
    expect(html).toMatch(/provider-card__stat is-empty[^>]*>—</);
  });

  it('labels the chain position and shows the status pill', async () => {
    const html = await renderCard({
      card: card(),
      health: { name: 'anthropic', status: 'healthy', disabled: false, chainPosition: 1 },
    });
    expect(html).toContain('Chain #1');
    expect(html).toContain('Healthy');
  });

  it('carries the usage bar and its caption when a share is known', async () => {
    const html = await renderCard({
      card: card(),
      health: { name: 'anthropic', status: 'healthy', disabled: false, usageShare: 0.253 },
    });
    expect(html).toContain('25.3% of usage (last 7 days)');
    expect(html).toContain('provider-card__usage-fill');
  });

  it('hides the usage row entirely when the share is unknown', async () => {
    const html = await renderCard({
      card: card(),
      health: { name: 'anthropic', status: 'healthy', disabled: false, usageShare: null },
    });
    expect(html).not.toContain('provider-card__usage');
  });
});

describe('provider card state visuals', () => {
  it('dims a disabled card and marks it stopped in the footer', async () => {
    const html = await renderCard({ card: card({ disabled: true }) });
    expect(html).toContain('provider-card--disabled');
    expect(html).toContain('provider-card__footer--centered');
    expect(html).toContain('Disabled');
    expect(html).not.toContain('provider-card--accent-error');
  });

  it('gives an errored card the error stripe and the probe’s own words', async () => {
    const html = await renderCard({
      card: card(),
      health: { name: 'anthropic', status: 'error', disabled: false },
      probeMessage: 'HTTP 401 Unauthorized',
    });
    expect(html).toContain('provider-card--accent-error');
    expect(html).toContain('provider-card__footer--error');
    expect(html).toContain('HTTP 401 Unauthorized');
  });

  it('gives a degraded card the warning stripe, not the error one', async () => {
    const html = await renderCard({
      card: card(),
      health: { name: 'anthropic', status: 'degraded', disabled: false },
    });
    expect(html).toContain('provider-card--accent-degraded');
    expect(html).not.toContain('provider-card--accent-error');
  });

  it('leaves a healthy card undecorated, with an empty footer', async () => {
    const html = await renderCard({
      card: card(),
      health: { name: 'anthropic', status: 'healthy', disabled: false },
    });
    expect(html).toContain('provider-card--healthy');
    expect(html).not.toContain('provider-card--accent');
    expect(html).toContain('provider-card__footer--none');
  });

  it('says offline for a card that was never probed', async () => {
    const html = await renderCard({
      card: card(),
      health: { name: 'anthropic', status: 'offline', disabled: false },
    });
    expect(html).toContain('Offline');
  });

  it('announces the card menu for keyboard and touch users', async () => {
    const html = await renderCard({ card: card() });
    expect(html).toContain('aria-haspopup="menu"');
    expect(html).toContain('role="group"');
    expect(html).toContain('tabindex="0"');
  });
});

describe('card identity by the mock set', () => {
  it('covers every state the design has to render', () => {
    const states = MOCK_PROVIDERS.map((p) => p.status);
    expect(states).toContain('healthy');
    expect(states).toContain('degraded');
    expect(states).toContain('error');
    expect(states).toContain('disabled');
    expect(MOCK_CARDS).toHaveLength(MOCK_PROVIDERS.length);
    expect(MOCK_CARDS.some((c) => c.disabled)).toBe(true);
  });
});

describe('provider context menu', () => {
  const items = [
    { key: 'edit', label: 'Edit' },
    { key: 'duplicate', label: 'Duplicate Card' },
    { key: 'test', label: 'Test Connection' },
    { key: 'toggle', label: 'Disable' },
    { key: 'delete', label: 'Delete Provider', destructive: true, dividerBefore: true },
  ];

  /** The menu is teleported to `body`; SSR collects that content in the
   *  render context instead of inlining it. */
  async function renderMenu(props: { visible: boolean; items: typeof items }): Promise<string> {
    const context: { teleports?: Record<string, string> } = {};
    await renderToString(
      createSSRApp({
        render: () => h(ProviderContextMenu, { position: { x: 10, y: 10 }, ...props }),
      }).use(i18n),
      context,
    );
    return context.teleports?.body ?? '';
  }

  it('renders the five actions in order, with a separator before delete', async () => {
    const html = await renderMenu({ visible: true, items });

    expect(html).toContain('role="menu"');
    expect(html.match(/role="menuitem"/g)).toHaveLength(5);
    expect(html.match(/role="separator"/g)).toHaveLength(1);
    for (const label of ['Edit', 'Duplicate Card', 'Test Connection', 'Disable', 'Delete Provider']) {
      expect(html).toContain(label);
    }
    expect(html.indexOf('Edit')).toBeLessThan(html.indexOf('Duplicate Card'));
    expect(html.indexOf('Test Connection')).toBeLessThan(html.indexOf('Delete Provider'));
  });

  it('disables the rows a stopped card may not use', async () => {
    const stopped = items.map((item) =>
      item.key === 'duplicate' || item.key === 'test' ? { ...item, disabled: true } : item,
    );
    const html = await renderMenu({ visible: true, items: stopped });
    expect(html.match(/\bdisabled\b/g)).toHaveLength(2);
  });

  it('renders nothing while it is closed', async () => {
    const html = await renderMenu({ visible: false, items });
    expect(html).not.toContain('role="menu"');
  });
});
