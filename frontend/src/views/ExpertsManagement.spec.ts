import { describe, expect, it, vi } from 'vitest';
import { nextTick, ref } from 'vue';
import { usePromptEditor, MAX_EXPERT_PROMPT_CHARS } from '../composables/usePromptEditor';
// The view's source, as text — see the note at the top of
// `designLanguage.spec.ts`: this project has no DOM test environment, so the
// editor's *surface* (editable textarea, explicit save, source hint) is pinned
// against the source while its *behaviour* is exercised through the extracted
// `usePromptEditor` state machine below.
import source from './ExpertsManagement.vue?raw';

/**
 * RENG-93 — the expert detail dialog's prompt editor.
 *
 * The project has no DOM (no jsdom/happy-dom), so the edit → save → success
 * and save-failure → revert flows cannot be driven by typing into a real
 * textarea. They are extracted into `usePromptEditor` (draft / dirty / saving /
 * saved + the sync rules) and exercised here against Vue's reactivity
 * directly; the view's bindings are pinned against its source, the same way
 * `reviewHistoryDrawer.spec.ts` pins CSS the renderer cannot measure.
 */

function makeExpert(prompt: string, promptOverride = false) {
  return { id: 'security', prompt, promptOverride };
}

describe('usePromptEditor — edit, save, success', () => {
  it('initialises the draft from the expert and syncs the server echo on save', async () => {
    const expert = ref(makeExpert('file prompt', false));
    const save = vi.fn(async () => ({ prompt: 'new persona', promptOverride: true }));
    const editor = usePromptEditor(expert, save);

    // The drawer opened: the textarea holds the effective prompt.
    expect(editor.draft.value).toBe('file prompt');
    expect(editor.dirty.value).toBe(false);

    // The user edits; nothing is sent yet (no auto-save per keystroke).
    editor.setDraft('new persona');
    expect(editor.dirty.value).toBe(true);
    expect(save).not.toHaveBeenCalled();

    const ok = await editor.savePrompt();
    expect(ok).toBe(true);
    expect(save).toHaveBeenCalledWith('security', 'new persona');
    expect(expert.value.prompt, 'the server echo becomes the effective value').toBe('new persona');
    expect(expert.value.promptOverride).toBe(true);
    expect(editor.draft.value).toBe('new persona');
    expect(editor.dirty.value).toBe(false);
    expect(editor.saveState.value).toBe('saved');
    expect(editor.saving.value).toBe(false);
  });

  it('saving an empty prompt (a clear) round-trips and flips the override flag off', async () => {
    const expert = ref(makeExpert('override persona', true));
    const save = vi.fn(async () => ({ prompt: 'file prompt', promptOverride: false }));
    const editor = usePromptEditor(expert, save);

    editor.setDraft('');
    const ok = await editor.savePrompt();
    expect(ok).toBe(true);
    expect(save).toHaveBeenCalledWith('security', '');
    expect(expert.value.prompt, 'cleared: the config-file value applies again').toBe('file prompt');
    expect(expert.value.promptOverride).toBe(false);
    expect(editor.saveState.value).toBe('saved');
  });

  it('does nothing when the draft matches the server value', async () => {
    const expert = ref(makeExpert('file prompt'));
    const save = vi.fn(async () => ({ prompt: 'file prompt', promptOverride: false }));
    const editor = usePromptEditor(expert, save);

    expect(await editor.savePrompt()).toBe(false);
    expect(save).not.toHaveBeenCalled();

    // A user edit that is reverted to the server value is also a no-op.
    editor.setDraft('scratch');
    editor.setDraft('file prompt');
    expect(editor.dirty.value).toBe(false);
    expect(await editor.savePrompt()).toBe(false);
  });

  it('holds the save in flight: saving disables further saves while a PUT is pending', async () => {
    const expert = ref(makeExpert('file prompt'));
    let release!: () => void;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const save = vi.fn(async () => {
      await gate;
      return { prompt: 'saved text', promptOverride: true };
    });
    const editor = usePromptEditor(expert, save);

    editor.setDraft('saved text');
    const pending = editor.savePrompt();
    expect(editor.saving.value).toBe(true);
    // A second click while in flight is a no-op, not a duplicate PUT.
    await expect(editor.savePrompt()).resolves.toBe(false);
    expect(save).toHaveBeenCalledTimes(1);

    release();
    await pending;
    expect(editor.saving.value).toBe(false);
    expect(editor.saveState.value).toBe('saved');
  });
});

describe('usePromptEditor — a failed save reverts and says so', () => {
  it('reverts the draft to the server value and rethrows for the view to notify', async () => {
    const expert = ref(makeExpert('file prompt'));
    const save = vi.fn(async () => {
      throw new Error('HTTP 500');
    });
    const editor = usePromptEditor(expert, save);

    editor.setDraft('unsaved edit');
    await expect(editor.savePrompt()).rejects.toThrow('HTTP 500');

    expect(editor.draft.value, 'the draft reverts to the value the server holds').toBe(
      'file prompt'
    );
    expect(expert.value.prompt, 'the expert object is untouched by a failed save').toBe(
      'file prompt'
    );
    expect(editor.dirty.value).toBe(false);
    expect(editor.saveState.value).toBe('idle');
    expect(editor.saving.value).toBe(false);
  });

  it('a failed CLEAR also reverts: the override stays in effect', async () => {
    const expert = ref(makeExpert('override persona', true));
    const save = vi.fn(async () => {
      throw new Error('HTTP 500');
    });
    const editor = usePromptEditor(expert, save);

    editor.setDraft('');
    await expect(editor.savePrompt()).rejects.toThrow('HTTP 500');
    expect(editor.draft.value).toBe('override persona');
    expect(expert.value.prompt).toBe('override persona');
    expect(editor.saveState.value).toBe('idle');
  });
});

describe('usePromptEditor — a background refresh never clobbers a local edit', () => {
  it('follows a new server value while the draft is clean', async () => {
    const expert = ref(makeExpert('file prompt'));
    const save = vi.fn(async () => ({ prompt: 'file prompt', promptOverride: false }));
    const editor = usePromptEditor(expert, save);

    // The poll reconciles in place (`Object.assign` in useExperts) — same
    // object identity, new prompt.
    expert.value.prompt = 'changed elsewhere';
    await nextTick();
    expect(editor.draft.value, 'a clean editor follows the poll').toBe('changed elsewhere');
  });

  it('keeps an unsaved edit when a poll lands mid-edit', async () => {
    const expert = ref(makeExpert('file prompt'));
    const save = vi.fn(async () => ({ prompt: 'file prompt', promptOverride: false }));
    const editor = usePromptEditor(expert, save);

    editor.setDraft('my edit in progress');
    expert.value.prompt = 'changed elsewhere';
    await nextTick();
    expect(editor.draft.value, 'the in-progress edit survives the poll').toBe(
      'my edit in progress'
    );

    // And the "Saved" indicator clears the moment the user edits again.
    editor.saveState.value = 'saved';
    editor.setDraft('still editing');
    await nextTick();
    expect(editor.saveState.value).toBe('idle');
  });
});

describe('usePromptEditor — switching expert re-initialises the draft', () => {
  it('starts from the newly selected expert’s prompt', async () => {
    const expert = ref(makeExpert('security prompt'));
    const save = vi.fn(async () => ({ prompt: 'x', promptOverride: false }));
    const editor = usePromptEditor(expert, save);

    editor.setDraft('unsaved text');
    expert.value = makeExpert('docs prompt');
    await nextTick();
    expect(editor.draft.value, 'the draft follows the selection').toBe('docs prompt');
    expect(editor.saveState.value).toBe('idle');
    expect(editor.dirty.value).toBe(false);
  });
});

describe('ExpertsManagement.vue — the prompt editor surface', () => {
  it('renders the prompt as an editable textarea with the server-side maxlength', () => {
    const flat = source.replace(/\s+/g, ' ');
    expect(flat).toContain(':model-value="promptDraft"');
    expect(flat).toContain('@update:model-value="setPromptDraft"');
    expect(flat).toContain(':maxlength="MAX_EXPERT_PROMPT_CHARS"');
    expect(flat).toContain(':disabled="promptSaving"');
    // The prompt textarea itself is no longer read-only (the description
    // field below it still is — only the prompt block must be editable).
    const promptBlock = source.slice(
      source.indexOf(':model-value="promptDraft"'),
      source.indexOf('lastReviews')
    );
    expect(promptBlock).not.toContain('readonly');
    // The mirror of the Rust-side limit, so a 422 cannot even be typed.
    expect(MAX_EXPERT_PROMPT_CHARS).toBe(20000);
  });

  it('saves explicitly — a button, never a keystroke', () => {
    expect(source).toContain('experts.detail.promptSave');
    expect(source).toContain('experts.detail.promptSaving');
    expect(source).toContain('@click="onSavePrompt"');
    expect(source).not.toContain('@input="savePrompt');
  });

  it('labels the prompt’s source honestly', () => {
    expect(source).toContain('experts.detail.promptSourceConfig');
    expect(source).toContain('experts.detail.promptSourceOverride');
    expect(source).toContain('selectedExpert.promptOverride');
  });

  it('pauses the background poll while a prompt edit is unsaved', () => {
    const flat = source.replace(/\s+/g, ' ');
    expect(flat).toContain(
      'weightDragPending.value || savesInFlight.value > 0 || promptDirty.value'
    );
  });

  it('no longer knows the old preview label', () => {
    expect(source).not.toContain('promptPreview');
    expect(source).not.toContain('experts.detail.promptPreview');
  });
});
