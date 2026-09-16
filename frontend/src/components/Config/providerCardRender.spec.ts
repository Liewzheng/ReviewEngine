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
    metrics: { avgLatency: 'Avg Latency', avgTtfb: 'Avg Comm. Latency', requests: 'Usages', successRate: 'Success Rate' },
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
    expect(html).toContain('**********');
  });

  it('renders mock v3 shape: no avatar, no status pill, no bullet mask', async () => {
    const html = await renderCard({ card: card(), health: undefined });
    expect(html).not.toContain('provider-card__monogram');
    expect(html).not.toContain('••');
    expect(html).not.toContain('provider-card__status');
    // The URL / model / key rows are told apart by their own rules, and the
    // header closes with the drag affordance the mock shows on the right.
    expect(html).toContain('provider-card__row--url');
    expect(html).toContain('provider-card__row--model');
    expect(html).toContain('provider-card__row--key');
    expect(html).toContain('provider-card__grip');
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

  it('shows the communication latency once the server measures one', async () => {
    const html = await renderCard({
      card: card(),
      health: {
        name: 'anthropic',
        status: 'healthy',
        disabled: false,
        avgTtfbMs: 17690,
        avgLatencyMs: 21000,
      },
    });
    // One compact token, with the recorded call latency kept out of the
    // card. The measurement name + exact value live in the hover tooltip
    // (pinned by `statsRow` in providerCardState.spec.ts); SSR skips the
    // el-tooltip popper body, so the trigger text is what this assertion
    // owns.
    expect(html).toContain('17.7s');
    expect(html).not.toContain('21.0s');
  });

  it('renders an unmeasured statistic as an em dash in the muted colour', async () => {
    const html = await renderCard({
      card: card(),
      health: { name: 'anthropic', status: 'error', disabled: false, avgLatencyMs: null, requestCount: 128, successRate: 0.821 },
    });
    expect(html).toMatch(/provider-card__stat is-empty[^>]*>—</);
  });

  it('keeps the chain marker beside the name and takes the status off the right edge', async () => {
    const html = await renderCard({
      card: card(),
      health: { name: 'anthropic', status: 'healthy', disabled: false, chainPosition: 1 },
    });
    expect(html).toContain('Chain #1');
    // RENG-77: no pill on the right — a healthy card carries no dot either, so
    // its state is announced in the accessible name instead of drawn.
    expect(html).not.toContain('provider-card__health');
    expect(html).toContain('anthropic · Healthy');
  });

  it('marks a card that needs attention with a dot beside its name', async () => {
    const degraded = await renderCard({
      card: card(),
      health: { name: 'anthropic', status: 'degraded', disabled: false },
    });
    expect(degraded).toContain('provider-card__health--degraded');
    expect(degraded).toContain('anthropic · Degraded');
    const errored = await renderCard({
      card: card(),
      health: { name: 'anthropic', status: 'error', disabled: false },
    });
    expect(errored).toContain('provider-card__health--error');
  });

  it('carries the usage bar and no caption under it', async () => {
    const html = await renderCard({
      card: card(),
      health: { name: 'anthropic', status: 'healthy', disabled: false, usageShare: 0.253 },
    });
    expect(html).toContain('provider-card__usage-fill');
    expect(html).not.toContain('provider-card__usage-label');
    // The share is still reachable — on the bar, for hover and screen readers.
    expect(html).toContain('25.3% of usage (last 7 days)');
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
