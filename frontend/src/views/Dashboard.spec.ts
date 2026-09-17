import { beforeEach, describe, expect, it, vi } from 'vitest';
import { createSSRApp, type Ref } from 'vue';
import { renderToString } from '@vue/server-renderer';
import { createI18n } from 'vue-i18n';
import { ID_INJECTION_KEY, ZINDEX_INJECTION_KEY } from 'element-plus';
import Dashboard from './Dashboard.vue';
import en from '../i18n/locales/en';
import zhCN from '../i18n/locales/zh-CN';
import type { DashboardResponse } from '../services/dashboard';
import type { HealthStatus } from '../types/dashboard';

/**
 * RENG-97 — the Dashboard's System Health integration rows, rendered.
 *
 * The integration rows used to report `success` from mere entry presence
 * (backend) while the Configuration page showed the real 401. The rows now
 * carry the shared probe-cache verdict (`unknown` / `error` / `success` /
 * `offline`), and this test pins that each state renders through i18n — the
 * failure mode that would otherwise ship is a raw English backend string
 * (or worse, a vue-i18n key) reaching the screen.
 *
 * Rendered through the SSR renderer like the LLM Status spec: no DOM test
 * environment in this project. The page's data comes from a stubbed
 * `useDashboard` composable; the chart only builds in `onMounted`, which SSR
 * never runs.
 */

vi.mock('../composables/useDashboard', async () => {
  const { ref } = await import('vue');
  const data = ref<DashboardResponse | null>(null);
  return {
    useDashboard: () => ({
      data,
      loading: ref(false),
      error: ref<string | null>(null),
      lastUpdated: ref<string | null>(null),
      pollFailed: ref(false),
      refresh: async () => {},
    }),
    __state: { data },
  };
});

vi.mock('../composables/useTheme', async () => {
  const { ref } = await import('vue');
  return { useTheme: () => ({ isDark: ref(true) }) };
});

vi.mock('vue-router', () => ({
  useRouter: () => ({ push: vi.fn() }),
}));

interface MockState {
  data: Ref<DashboardResponse | null>;
}

async function state(): Promise<MockState> {
  const mocked = (await import('../composables/useDashboard')) as unknown as { __state: MockState };
  return mocked.__state;
}

/** A probe timestamp "just now", so the row renders a stable relative word. */
const NOW = new Date().toISOString();

function integration(over: Partial<HealthStatus> & { service: string }): HealthStatus {
  return {
    type: 'integration',
    status: 'unknown',
    ...over,
  };
}

function fixture(integrations: HealthStatus[]): DashboardResponse {
  return {
    kpis: null as never,
    trend: [],
    trendDaily: [],
    health: {
      integrations,
      llmProviders: [],
      overall: 'offline',
      lastChecked: NOW,
      llmConfigured: false,
    },
    recentReviews: [],
  };
}

async function renderDashboard(locale: 'en' | 'zh-CN'): Promise<string> {
  const i18n = createI18n({
    legacy: false,
    locale,
    messages: { [locale]: locale === 'zh-CN' ? zhCN : en },
  });
  const app = createSSRApp(Dashboard).use(i18n);
  // Element Plus wants deterministic id and z-index sources during SSR.
  app.provide(ID_INJECTION_KEY, { prefix: 1, current: 0 });
  app.provide(ZINDEX_INJECTION_KEY, { current: 0 });
  return await renderToString(app);
}

/** Everything the page shows, tags removed — where a raw i18n key or a raw
 *  backend string would appear. */
const visibleText = (html: string) => html.replace(/<[^>]+>/g, ' ').replace(/\s+/g, ' ');

describe('RENG-97 — the Dashboard System Health integration rows', () => {
  beforeEach(async () => {
    const s = await state();
    s.data.value = null;
  });

  it('renders an unprobed-but-configured integration as unknown, not success', async () => {
    const s = await state();
    s.data.value = fixture([integration({ service: 'GitLab API' })]);

    for (const [locale, expected] of [
      ['en', 'Unknown'],
      ['zh-CN', '未知'],
    ] as const) {
      const html = await renderDashboard(locale);
      const text = visibleText(html);
      expect(text, locale).toContain('GitLab API');
      expect(text, `${locale} must say the row was never probed`).toContain(expected);
      expect(text, `${locale} must render the reason`).toContain(
        locale === 'zh-CN' ? '未探测' : 'Not probed yet'
      );
      // The honest states never overstate health.
      expect(text, `${locale} claimed Configured on an unprobed entry`).not.toContain('Configured');
      expect(text, `${locale} printed an i18n key`).not.toContain('dashboard.health.');
      expect(text, `${locale} printed an i18n key`).not.toContain('common.status.');
    }
  });

  it('renders a failed probe with the failure and its timestamp', async () => {
    const s = await state();
    s.data.value = fixture([
      integration({
        service: 'GitLab API',
        status: 'error',
        message: 'HTTP 401 Unauthorized',
        checkedAt: NOW,
      }),
    ]);

    const html = await renderDashboard('en');
    const text = visibleText(html);
    expect(text).toContain('Error');
    expect(text).toContain('HTTP 401 Unauthorized');
    expect(text).toContain('Last test: just now');
  });

  it('renders a successful probe with its latency detail', async () => {
    const s = await state();
    s.data.value = fixture([
      integration({
        service: 'GitLab API',
        status: 'success',
        message: 'Configured',
        latencyMs: 37,
        checkedAt: NOW,
      }),
      integration({ service: 'GitHub API', status: 'offline' }),
    ]);

    const html = await renderDashboard('en');
    const text = visibleText(html);
    expect(text).toContain('Operational');
    expect(text).toContain('Configured');
    expect(text).toContain('Last test: just now');
    // The unconfigured sibling stays honestly offline.
    expect(text).toContain('GitHub API');
    expect(text).toContain('Not configured');
  });

  it('renders an offline integration with the not-configured reason', async () => {
    const s = await state();
    s.data.value = fixture([integration({ service: 'GitLab API', status: 'offline' })]);

    const html = await renderDashboard('zh-CN');
    const text = visibleText(html);
    expect(text).toContain('离线');
    expect(text).toContain('未配置');
  });
});
