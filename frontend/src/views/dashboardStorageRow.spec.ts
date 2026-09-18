import { beforeEach, describe, expect, it, vi } from 'vitest';
import { createSSRApp, type Ref } from 'vue';
import { renderToString } from '@vue/server-renderer';
import { createI18n } from 'vue-i18n';
import { ID_INJECTION_KEY, ZINDEX_INJECTION_KEY } from 'element-plus';
import Dashboard from './Dashboard.vue';
// The template as text — see the note at the top of `designLanguage.spec.ts`:
// this project has no DOM to measure, so template wiring is pinned at source.
import dashboardSource from './Dashboard.vue?raw';
import en from '../i18n/locales/en';
import zhCN from '../i18n/locales/zh-CN';
import zhTW from '../i18n/locales/zh-TW';
import ja from '../i18n/locales/ja';
import ko from '../i18n/locales/ko';
import fr from '../i18n/locales/fr';
import type { DashboardResponse } from '../services/dashboard';
import type { StorageHealth } from '../types/dashboard';

/**
 * RENG-106 — the Dashboard's 存储 row.
 *
 * A host-side process changed the owner of `review.db-wal` / `review.db-shm`
 * on the user's NAS, so SQLite in WAL mode could not write its sidecars: every
 * database write failed with `attempt to write a readonly database`, reviews
 * ran and posted to GitLab but never persisted, and the UI showed nothing at
 * all. The card now carries the verdict of a REAL write test (a cached
 * `PRAGMA user_version` write) beside the integration and LLM rows, with the
 * failure's own words — sqlite's error plus the sidecar ownership cause — and
 * a hint pointing at `reng doctor --fix`.
 *
 * Two things are pinned here, both of which have failed silently in this
 * project before: the wiring (a row that renders nothing if the response
 * shape is mistyped or the section is dropped) and the i18n coverage across
 * all six locales. Rendering goes through the SSR renderer, like
 * `Dashboard.spec.ts`: there is no DOM test environment.
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

function fixture(storage: StorageHealth | undefined): DashboardResponse {
  return {
    kpis: null as never,
    trend: [],
    trendDaily: [],
    health: {
      integrations: [],
      llmProviders: [],
      overall: storage?.status === 'error' ? 'error' : 'success',
      lastChecked: NOW,
      llmConfigured: true,
      storage,
    },
    recentReviews: [],
  };
}

const LOCALES = { en, 'zh-CN': zhCN, 'zh-TW': zhTW, ja, ko, fr } as const;

async function renderDashboard(locale: keyof typeof LOCALES): Promise<string> {
  const i18n = createI18n({
    legacy: false,
    locale,
    messages: { [locale]: LOCALES[locale] },
  });
  const app = createSSRApp(Dashboard).use(i18n);
  // Element Plus wants deterministic id and z-index sources during SSR.
  app.provide(ID_INJECTION_KEY, { prefix: 1, current: 0 });
  app.provide(ZINDEX_INJECTION_KEY, { current: 0 });
  return await renderToString(app);
}

/** Everything the page shows, tags removed — where a raw i18n key or an
 *  unexpected raw backend string would appear. */
const visibleText = (html: string) => html.replace(/<[^>]+>/g, ' ').replace(/\s+/g, ' ');

const READONLY = 'attempt to write a readonly database';

describe('RENG-106 — the Dashboard 存储 row is wired', () => {
  it('renders the section and the 数据库 row from the response', () => {
    expect(dashboardSource).toContain("dashboard.health.storage");
    expect(dashboardSource).toContain("dashboard.health.database");
    expect(dashboardSource).toContain('v-if="health.storage"');
    // The category title is a section of its own, with the row inside it.
    expect(dashboardSource).toMatch(/health-section-title">\{\{ \$t\('dashboard\.health\.storage'\) \}\}/);
  });

  it('truncates the message with EllipsisText and keeps the timestamp fully visible', () => {
    // Same treatment as the integration/LLM rows (RENG-103): the message may
    // be a full sqlite error plus the ownership cause, so it is the span that
    // shrinks; the checked-at line keeps its natural width.
    expect(dashboardSource).toContain('<EllipsisText :text="storageMessage(health.storage)" />');
    expect(dashboardSource).toContain(":text=\"$t('common.lastTest', { date: timeAgo(health.storage.checkedAt) })\"");
    expect(dashboardSource).toMatch(
      /class="health-latency health-latency-ts">\s*<EllipsisText :text="\$t\('common\.lastTest'[\s\S]*?health\.storage\.checkedAt/
    );
    // The row reuses the card's existing geometry — no new row layout to keep
    // in sync.
    expect(dashboardSource).toContain('class="health-row last-row"');
  });

  it('maps the backend vocabulary onto the badge and i18n\'s the healthy state', () => {
    // `StatusBadge` speaks success/error; the API says healthy/error.
    expect(dashboardSource).toContain("return item.status === 'healthy' ? 'success' : 'error'");
    expect(dashboardSource).toContain("t('dashboard.health.storageWritable')");
    // The hint is a failure-only remedy, never noise on a healthy install.
    expect(dashboardSource).toContain("v-if=\"health.storage.status === 'error'\"");
    expect(dashboardSource).toContain('{{ $t(\'dashboard.health.storageHint\') }}');
  });

  it('declares every i18n key the section uses, in all six locales', () => {
    for (const [locale, messages] of Object.entries(LOCALES)) {
      const health = (messages as { dashboard: { health: Record<string, string> } }).dashboard.health;
      for (const key of ['storage', 'database', 'storageWritable', 'storageHint']) {
        expect(health[key], `${locale} is missing dashboard.health.${key}`).toBeTruthy();
      }
      // The remedy names the command it tells the user to run.
      expect(health.storageHint, `${locale} hint must name the command`).toContain('reng doctor --fix');
    }
    // zh-CN is the deployment's first language: the section reads as agreed.
    expect((zhCN as { dashboard: { health: Record<string, string> } }).dashboard.health.storage).toBe('存储');
    expect((zhCN as { dashboard: { health: Record<string, string> } }).dashboard.health.database).toBe('数据库');
  });
});

describe('RENG-106 — the 存储 row as rendered', () => {
  beforeEach(async () => {
    const s = await state();
    s.data.value = null;
  });

  it('shows a passing write test as operational, with no hint', async () => {
    const s = await state();
    s.data.value = fixture({ status: 'healthy', message: 'Write test passed', checkedAt: NOW });

    for (const [locale, expected] of [
      ['en', ['Storage', 'Database', 'Write test passed', 'Operational', 'Last test: just now']],
      ['zh-CN', ['存储', '数据库', '写入测试通过', '运行正常', '上次测试：刚刚']],
    ] as const) {
      const text = visibleText(await renderDashboard(locale));
      for (const needle of expected) {
        expect(text, `${locale} must render ${needle}`).toContain(needle);
      }
      expect(text, `${locale} printed an i18n key`).not.toContain('dashboard.health.');
      expect(text, `${locale} printed an i18n key`).not.toContain('common.status.');
      // No remedy on a healthy install.
      expect(text, `${locale} showed the hint while healthy`).not.toContain('reng doctor --fix');
    }
  });

  it('shows a failing write test with sqlite\'s words, the cause and the remedy', async () => {
    const s = await state();
    const message =
      'attempt to write a readonly database（review.db-wal 属主 uid 1026，当前进程 uid 9001 无法写入）';
    s.data.value = fixture({ status: 'error', message, checkedAt: NOW });

    const enText = visibleText(await renderDashboard('en'));
    expect(enText).toContain('Database');
    // The failure keeps the probe's own words — never a friendly euphemism.
    expect(enText).toContain(READONLY);
    expect(enText).toContain('uid 1026');
    expect(enText).toContain('Error');
    expect(enText).toContain('reng doctor --fix');

    const zhText = visibleText(await renderDashboard('zh-CN'));
    expect(zhText).toContain('存储');
    expect(zhText).toContain('数据库');
    expect(zhText).toContain('错误');
    expect(zhText).toContain(READONLY);
    expect(zhText).toContain('reng doctor --fix');
    expect(zhText).not.toContain('dashboard.health.');
  });

  it('renders every locale without a raw i18n key', async () => {
    const s = await state();
    s.data.value = fixture({
      status: 'error',
      message: 'attempt to write a readonly database',
      checkedAt: NOW,
    });
    for (const locale of Object.keys(LOCALES) as (keyof typeof LOCALES)[]) {
      const text = visibleText(await renderDashboard(locale));
      expect(text, `${locale} printed an i18n key`).not.toContain('dashboard.health.');
      expect(text, `${locale} printed an i18n key`).not.toContain('common.status.');
      expect(text, `${locale} must show the failure`).toContain(READONLY);
    }
  });

  it('hides the section entirely when the server predates the field', async () => {
    const s = await state();
    s.data.value = fixture(undefined);
    const text = visibleText(await renderDashboard('en'));
    expect(text).not.toContain('Storage');
    expect(text).not.toContain('Database');
  });
});
