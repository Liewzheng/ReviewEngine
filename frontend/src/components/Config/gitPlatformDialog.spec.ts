import { describe, expect, it } from 'vitest';
import { createSSRApp, h } from 'vue';
import { renderToString } from '@vue/server-renderer';
import { createI18n } from 'vue-i18n';
import { ID_INJECTION_KEY, ZINDEX_INJECTION_KEY } from 'element-plus';
import type { GitPlatformConfig } from '../../types/config';
import { CLEAR_SECRET_SENTINEL } from '../../types/config';
import {
  clearable,
  editDraftSecrets,
  SECRET_MASK,
  secretDisplay,
  showPasswordFor,
  submittedSecret,
} from './gitPlatformDialogState';
import GitPlatformForm from './GitPlatformForm.vue';
import GitPlatformsSection from './GitPlatformsSection.vue';

/**
 * RENG-96 dialog secret contract. Rendered for real through the SSR renderer
 * (the project has no DOM test environment — see providerCardRender.spec.ts).
 * The el-dialog shell is not SSR-inspectable (element-plus gates its body on
 * a mount-time `rendered` flag), so the form — the part with the mask/clear
 * behavior — is rendered directly; the pure state transitions
 * (`gitPlatformDialogState`) are unit-tested separately, including the clear
 * affordance that a static snapshot cannot click.
 */

const messages = {
  common: { optional: 'optional', cancel: 'Cancel', save: 'Save', edit: 'Edit', remove: 'Remove' },
  errors: { unknown: 'unknown error' },
  config: {
    notSet: 'Not set',
    validation: { invalidUrl: 'Invalid URL' },
    gitPlatforms: {
      title: 'Git Platforms',
      addBtn: 'Add Platform',
      empty: 'No Git platforms configured yet',
      name: 'Name',
      namePlaceholder: 'e.g. gitlab-main',
      type: 'Type',
      baseUrl: 'Base URL',
      baseUrlPlaceholder: 'https://gitlab.example.com',
      internalBaseUrl: 'Internal URL',
      internalBaseUrlPlaceholder: 'Internally reachable URL (optional)',
      internalBaseUrlHelp: 'Internal URL help',
      token: 'Access Token',
      tokenPlaceholder: 'glpat-...',
      keepTokenPlaceholder: 'Leave empty to keep the saved token',
      webhookSecret: 'Secret token',
      webhookSigningSecret: 'Signing token',
      keepSecretPlaceholder: 'Leave empty to keep the saved value',
      webhookSecretHelp: 'Secret token help',
      webhookSigningSecretHelp: 'Signing token help',
      allowedProjects: 'Allowed projects',
      allowedProjectsPlaceholder: 'One project path per line',
      allowedProjectsHelp: 'Allowlist help',
      test: 'Test',
      addDialogTitle: 'Add Git Platform',
      editDialogTitle: 'Edit Git Platform',
      nameRequired: 'Please enter a name',
      nameDuplicate: 'A platform with this name already exists',
      baseUrlRequired: 'Please enter the base URL',
      clearSecret: 'Clear',
    },
  },
};

const i18n = createI18n({ legacy: false, locale: 'en', messages: { en: messages } });

function configuredPlatform(over: Partial<GitPlatformConfig> = {}): GitPlatformConfig {
  return {
    id: '5e3a1c8e-0000-4000-8000-000000000001',
    name: 'testbed',
    type: 'gitlab',
    baseUrl: 'http://gitlab.internal:8929',
    internalBaseUrl: '',
    token: SECRET_MASK,
    webhookSecret: SECRET_MASK,
    webhookSigningSecret: '',
    allowedProjects: [],
    ...over,
  };
}

async function renderForm(props: {
  mode: 'add' | 'edit';
  platform?: GitPlatformConfig;
  platforms: GitPlatformConfig[];
  editingIndex: number;
}): Promise<string> {
  const app = createSSRApp({
    render: () => h(GitPlatformForm, { ...props }),
  }).use(i18n);
  // Element Plus warns without deterministic id/z-index sources during SSR.
  app.provide(ID_INJECTION_KEY, { prefix: 1, current: 0 });
  app.provide(ZINDEX_INJECTION_KEY, { current: 0 });
  return renderToString(app);
}

describe('git platform dialog secret state', () => {
  it('builds the edit draft with the mask as the visible value', () => {
    const draft = editDraftSecrets(configuredPlatform());
    expect(draft.token).toBe(SECRET_MASK);
    expect(draft.webhookSecret).toBe(SECRET_MASK);
    expect(draft.webhookSigningSecret).toBe(''); // unconfigured stays empty
    expect(editDraftSecrets(configuredPlatform({ token: '', webhookSecret: '' }))).toEqual({
      token: '',
      webhookSecret: '',
      webhookSigningSecret: '',
    });
  });

  it('shows the mask literally, the clear sentinel as an empty box', () => {
    expect(secretDisplay(SECRET_MASK)).toBe(SECRET_MASK);
    expect(secretDisplay(CLEAR_SECRET_SENTINEL)).toBe('');
    expect(secretDisplay('glpat-real')).toBe('glpat-real');
    expect(secretDisplay('')).toBe('');
  });

  it('password-dots a real value but renders the mask in plain text', () => {
    expect(showPasswordFor(SECRET_MASK)).toBe(false);
    expect(showPasswordFor('glpat-real')).toBe(true);
    expect(showPasswordFor(CLEAR_SECRET_SENTINEL)).toBe(true);
    expect(showPasswordFor('')).toBe(true);
  });

  it('offers the clear button only while the stored mask is still shown', () => {
    expect(clearable(SECRET_MASK)).toBe(true);
    expect(clearable(CLEAR_SECRET_SENTINEL)).toBe(false); // already cleared
    expect(clearable('glpat-real')).toBe(false); // typing a replacement hides it
    expect(clearable('')).toBe(false);
  });

  it('submits keep/clear/replace per the backend contract', () => {
    // Untouched mask → mask (keep); emptied box → the echoed original (keep).
    expect(submittedSecret(SECRET_MASK, SECRET_MASK)).toBe(SECRET_MASK);
    expect(submittedSecret('', SECRET_MASK)).toBe(SECRET_MASK);
    expect(submittedSecret('', '')).toBe('');
    // Explicit clear sentinel passes through to the backend.
    expect(submittedSecret(CLEAR_SECRET_SENTINEL, SECRET_MASK)).toBe(CLEAR_SECRET_SENTINEL);
    // A typed value replaces.
    expect(submittedSecret('glpat-new', SECRET_MASK)).toBe('glpat-new');
  });
});

describe('git platform form rendering', () => {
  it('shows the `***` mask for a configured secret — never an empty box', async () => {
    // el-input sets its DOM value imperatively (a mount-time watch), so SSR
    // cannot show the `***` value attribute — the pure-state tests pin the
    // exact value instead. What SSR CAN prove: the configured token renders
    // as literal TEXT (the mask, not password dots), carries no "leave empty
    // to keep" hint (the mask fills the box), and keeps the clear button.
    const html = await renderForm({
      mode: 'edit',
      platform: configuredPlatform(),
      platforms: [configuredPlatform()],
      editingIndex: 0,
    });
    // Only the UNCONFIGURED signing field is password-dotted; both configured
    // secrets render as plain text — the mask is what the box shows.
    expect(html.match(/type="password"/g)?.length).toBe(1);
    expect(html).not.toContain('Leave empty to keep the saved token');
    // The genuinely-empty signing field still carries the keep hint.
    expect(html).toContain('Leave empty to keep the saved value');
    expect(html).toContain('aria-label="Clear"');
  });

  it('shows one clear button per configured secret and none for an unset one', async () => {
    const html = await renderForm({
      mode: 'edit',
      platform: configuredPlatform({ webhookSigningSecret: '' }),
      platforms: [configuredPlatform()],
      editingIndex: 0,
    });
    // Token and webhook secret are configured → one clear button each.
    expect(html.match(/aria-label="Clear"/g)?.length).toBe(2);
    expect(html.match(/>Clear</g)?.length).toBe(2);
  });

  it('renders the add form without masks or clear buttons', async () => {
    const html = await renderForm({
      mode: 'add',
      platforms: [],
      editingIndex: -1,
    });
    expect(html).not.toContain('Clear');
    expect(html).toContain('glpat-...');
  });
});

describe('git platforms section list rendering', () => {
  async function renderSection(platforms: GitPlatformConfig[]): Promise<string> {
    const app = createSSRApp({
      render: () => h(GitPlatformsSection, { platforms }),
    }).use(i18n);
    // Element Plus warns without deterministic id/z-index sources during SSR.
    app.provide(ID_INJECTION_KEY, { prefix: 1, current: 0 });
    app.provide(ZINDEX_INJECTION_KEY, { current: 0 });
    return renderToString(app);
  }

  it('marks a row with a configured token as set (masked, never a value)', async () => {
    const html = await renderSection([configuredPlatform()]);
    expect(html).toContain('platform-item-token is-set');
    expect(html).not.toContain('glpat-platform');
  });

  it('labels a token-less row as not set', async () => {
    const html = await renderSection([configuredPlatform({ token: '' })]);
    expect(html).not.toContain('platform-item-token is-set');
    expect(html).toContain('Not set');
  });
});
