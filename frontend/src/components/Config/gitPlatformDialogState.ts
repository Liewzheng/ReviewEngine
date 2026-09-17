/**
 * Pure state transitions for the Git platform add/edit dialog's secret
 * fields (RENG-96). Kept free of Vue/DOM so the mask/keep/clear contract is
 * unit-testable in the project's SSR-only test environment — the component
 * wiring lives in `GitPlatformDialog.vue` / `GitPlatformsSection.vue`.
 */
import type { GitPlatformConfig } from '../../types/config'
import { CLEAR_SECRET_SENTINEL } from '../../types/config'

/** GET /config masks a configured secret with the backend's API_KEY_MASK. */
export const SECRET_MASK = '***'

/** The three secret fields of the git platform form. */
export type SecretField = 'token' | 'webhookSecret' | 'webhookSigningSecret'

/** True when the backend reported a stored secret (masked echo). */
export const isMasked = (value: string) => value === SECRET_MASK

/** What a secret input SHOWS: the mask renders as itself (`***`, never an
 * empty box), the clear sentinel renders as an empty box (the stored value
 * is being removed). */
export const secretDisplay = (value: string) => (value === CLEAR_SECRET_SENTINEL ? '' : value)

/** Password-dot a real value; render the `***` mask literally so a
 * configured secret is visible as asterisks instead of an empty box. */
export const showPasswordFor = (value: string) => value !== SECRET_MASK

/** The explicit-clear button is available while the stored mask is still
 * shown (before the user typed a replacement or clicked Clear). */
export const clearable = (value: string) => value === SECRET_MASK

/** The edit draft's secret fields: the mask when a value is configured,
 * `''` when not — the mask IS the visible value, not a placeholder. */
export function editDraftSecrets(platform: GitPlatformConfig): Record<SecretField, string> {
  return {
    token: platform.token ?? '',
    webhookSecret: platform.webhookSecret ?? '',
    webhookSigningSecret: platform.webhookSigningSecret ?? '',
  }
}

/** The value actually submitted for a secret field:
 * - the clear sentinel passes through (backend clears the stored secret);
 * - an empty draft (the user deleted the mask) falls back to the echoed
 *   original — `***`/`''` — so "blank = keep" still holds;
 * - anything else (a typed value, or the untouched mask) submits as-is.
 */
export function submittedSecret(draftValue: string, originalValue: string): string {
  if (draftValue === CLEAR_SECRET_SENTINEL) return CLEAR_SECRET_SENTINEL
  if (!draftValue) return originalValue
  return draftValue
}
