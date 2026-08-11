import { request } from './api';

/**
 * Shape of `GET /api/v1/system/auth-status`.
 *
 * Deliberately open (unauthenticated): reveals only booleans, never the token
 * itself, so the app can decide between the first-run bootstrap screen and
 * normal operation before it has any credentials.
 */
export interface AuthStatus {
  /** A token is configured server-side (persisted in auth.toml). */
  configured: boolean;
  /** True while no token is configured — the first-run bootstrap window. */
  bootstrap: boolean;
  /**
   * First-run setup on a non-loopback bind (Docker / public deployments)
   * requires the one-time bootstrap key; loopback (local dev) needs none.
   */
  bootstrapKeyRequired: boolean;
}

/**
 * Probe whether an API token is configured. Call before any authenticated
 * request so the app can show the first-run bootstrap screen when
 * `configured === false`, instead of a generic login dialog.
 */
export function getAuthStatus(): Promise<AuthStatus> {
  return request('/system/auth-status');
}

/**
 * Set or rotate the API auth token on the server. The digest is persisted to
 * auth.toml and the in-memory value hot-swaps, so the new token takes effect
 * immediately (no restart).
 *
 * Auth contract (enforced by the backend middleware):
 * - No token yet (first-run bootstrap): open from a loopback bind; on a
 *   non-loopback bind it requires `bootstrapKey` (the one-time key from
 *   REVIEW_BOOTSTRAP_KEY / --bootstrap-key) via the `X-Bootstrap-Key` header.
 * - A token already configured: the caller must authenticate with the current
 *   token — `request` adds `Authorization: Bearer <cached token>` automatically.
 *
 * @returns the parsed response, `{status: 'saved', configured: true}` on success.
 */
export function setSystemToken(
  token: string,
  bootstrapKey?: string
): Promise<{ status: string; configured: boolean }> {
  const headers: Record<string, string> = {};
  if (bootstrapKey) {
    headers['X-Bootstrap-Key'] = bootstrapKey;
  }
  return request('/system/token', {
    method: 'PUT',
    headers,
    body: JSON.stringify({ token }),
  });
}
