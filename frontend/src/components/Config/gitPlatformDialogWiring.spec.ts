import { describe, expect, it } from 'vitest';
import dialogSource from './GitPlatformDialog.vue?raw';

/**
 * Regression pin for the RENG-100 hotfix (0.10.43).
 *
 * RENG-96 split the git-platform editor into `GitPlatformDialog` (the
 * el-dialog shell, owner of the footer's Save button and of the draft) and
 * `GitPlatformForm` (the form itself, exposing `save()` via `defineExpose`).
 * The shell then called `formRef?.save()` — but the `<GitPlatformForm>`
 * element never carried `ref="formRef"`, so `formRef.value` stayed
 * `undefined` and the optional call swallowed the click: **Save did nothing,
 * silently**, on the one screen whose entire purpose is to store credentials.
 *
 * The suite has no DOM to click in (the project renders through SSR), so this
 * pins the wiring at the source level, the same way `designLanguage.spec.ts`
 * pins other DOM-less contracts: a ref a template calls must be bound in that
 * template.
 */

const template = dialogSource.match(/<template>([\s\S]*)<\/template>/)?.[1] ?? '';

describe('RENG-100 — the dialog shell binds the ref its Save button calls', () => {
  it('carries the form the footer triggers', () => {
    expect(template).toMatch(/<GitPlatformForm[^>]*\bref="formRef"/s);
  });

  it('calls the form ref from the footer button', () => {
    expect(template).toMatch(/@click="formRef\?\.save\(\)"/);
  });

  it('declares the ref it calls', () => {
    expect(dialogSource).toMatch(/const\s+formRef\s*=\s*ref/);
  });

  /**
   * The general shape that produced the bug: an `xxxRef?.…` call in a
   * template needs a matching `ref="xxxRef"` binding, or the optional call
   * hides the missing link. Checked for every ref this component declares.
   */
  it('binds every ref this component calls from its template', () => {
    const declared = [...dialogSource.matchAll(/const\s+([A-Za-z_][A-Za-z0-9_]*Ref)\s*=\s*ref/g)].map((m) => m[1]);
    expect(declared.length).toBeGreaterThan(0);
    for (const name of declared) {
      if (new RegExp(`${name}\\?\\.`).test(template)) {
        expect(template, `${name} is called but never bound`).toContain(`ref="${name}"`);
      }
    }
  });
});
