import { computed, ref, watch, type Ref } from 'vue';

/**
 * The maximum prompt length the server accepts — the textarea's `maxlength`
 * mirrors the Rust side's `MAX_EXPERT_PROMPT_CHARS` so a prompt the API would
 * reject with 422 cannot even be typed here. The server stays the authority:
 * if the two ever drift, the PUT fails and the drawer reverts.
 */
export const MAX_EXPERT_PROMPT_CHARS = 20000;

/** The fields a successful save must echo back, so the editor can sync the
 *  effective value and its source honestly. */
export interface PromptSaveResult {
  prompt: string;
  promptOverride: boolean;
}

/**
 * RENG-93: the expert detail dialog's prompt editor state machine.
 *
 * The textarea edits a local `draft`, never the store's expert directly —
 * that is what keeps a background refresh from clobbering an unsaved edit
 * (the poll only reconciles the expert object; the draft is the editor's
 * own). Saving is explicit: `savePrompt` PUTs the draft and, on success,
 * syncs the server echo back onto the expert; on failure it reverts the
 * draft to the value the server still holds and rethrows, so the view can
 * notify.
 *
 * User input is distinguished from the editor's own syncs via `touched`:
 * `setDraft` (the textarea's update handler) marks the draft as user-edited,
 * while the follow/init/save writes do not. The follow rule is then "follow a
 * new server value unless the user has an unsaved edit" — a poll can never
 * overwrite a local edit, and a server-side change under a clean editor is
 * followed instead of being mistaken for an edit the user must reconcile.
 *
 * The view supplies the persistence call; this composable only owns the
 * editor's state (draft / dirty / saving / saved) and the sync rules.
 */
export function usePromptEditor(
  expert: Ref<{ id: string; prompt: string; promptOverride?: boolean } | null>,
  save: (id: string, prompt: string) => Promise<PromptSaveResult>
) {
  /** The textarea's text — the user's unsaved edit while `dirty`. */
  const draft = ref('');
  /** True while a save is in flight (disables the editor + save button). */
  const saving = ref(false);
  /** 'saved' for a short while after a successful save (inline feedback). */
  const saveState = ref<'idle' | 'saved'>('idle');
  /** True once the user edits the draft; cleared by the editor's own syncs.
   *  `dirty` is `touched && draft ≠ server`, so a server-side change under a
   *  clean editor neither arms the Save button nor pauses the poll. */
  let touched = false;

  /** The draft differs from the prompt the server last confirmed, AND the
   *  difference is the user's — there is something worth saving.
   *
   *  The reactive reads (`expert.value`, `e.prompt`, `draft.value`) happen
   *  BEFORE the `touched` gate so the computed always subscribes to them: a
   *  short-circuit on the non-reactive `touched` would leave the computed
   *  blind to a later `draft` write and it would answer with a stale cache. */
  const dirty = computed(() => {
    const e = expert.value;
    if (!e) return false;
    const prompt = e.prompt;
    const text = draft.value;
    return touched && text !== prompt;
  });

  // (Re)initialise the draft when the expert changes — dialog open, or a
  // different expert selected while it is open.
  watch(
    expert,
    (e) => {
      draft.value = e?.prompt ?? '';
      touched = false;
      saveState.value = 'idle';
    },
    { immediate: true }
  );

  // Follow a background refresh's new server value only while the user has no
  // unsaved edit: a local edit is never clobbered, and once saved the server
  // echo is what the draft holds anyway.
  watch(
    () => expert.value?.prompt,
    (p) => {
      if (p !== undefined && !touched) draft.value = p;
    }
  );

  /** The textarea's update handler: every user keystroke. */
  function setDraft(value: string): void {
    draft.value = value;
    touched = true;
    if (dirty.value) saveState.value = 'idle';
  }

  /**
   * Save the draft. Resolves `true` on success; on failure the draft is
   * reverted to the expert's current prompt and the error is rethrown for the
   * view to surface. Returns `false` (no-op) when there is nothing to save.
   */
  async function savePrompt(): Promise<boolean> {
    const e = expert.value;
    if (!e || !dirty.value || saving.value) return false;
    const previous = e.prompt;
    saving.value = true;
    try {
      const updated = await save(e.id, draft.value);
      e.prompt = updated.prompt;
      if (updated.promptOverride !== undefined) e.promptOverride = updated.promptOverride;
      draft.value = updated.prompt;
      touched = false;
      saveState.value = 'saved';
      return true;
    } catch (err) {
      draft.value = previous;
      saveState.value = 'idle';
      throw err;
    } finally {
      saving.value = false;
    }
  }

  return { draft, dirty, saving, saveState, setDraft, savePrompt };
}
