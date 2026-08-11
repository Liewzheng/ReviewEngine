const BASE_URL = '/api/v1';
const LS_TOKEN_KEY = 'review_engine_api_token';

/**
 * Error thrown by {@link request} for non-2xx responses. Carries the numeric
 * HTTP status plus, when the backend provides one, the machine-readable
 * `code` parsed from the JSON error body (`{"code": "..."}`) — e.g.
 * `auth_required`, `bootstrap_key_required`, or `unauthorized`.
 */
export type ApiError = Error & {
  status?: number;
  code?: string;
};

type AuthSignalHandler = (code: string) => void;

const authSignalHandlers = new Set<AuthSignalHandler>();

/**
 * Register a handler for auth-related 401 signals parsed from JSON error
 * bodies. Dispatched codes:
 * - `auth_required` — no token configured server-side; the app must show the
 *   first-run bootstrap screen.
 * - `unauthorized` — a token is configured but the request did not carry a
 *   valid one; the app must prompt for the existing token.
 * - `bootstrap_key_required` — first token on a non-loopback bind needs the
 *   one-time bootstrap key (handled inline by the bootstrap screen).
 *
 * A single consumer (App.vue) drives the matching UI; services stay decoupled
 * from Vue by emitting here instead of importing components.
 */
export function onAuthSignal(handler: AuthSignalHandler): void {
  authSignalHandlers.add(handler);
}

function dispatchAuthSignal(code: string): void {
  authSignalHandlers.forEach((handler) => handler(code));
}

/**
 * Read the current API token from browser localStorage.
 *
 * Token policy:
 * - The token is loaded from `localStorage.getItem('review_engine_api_token')`.
 * - There is no `/config.json` fallback, so the token is never embedded in the
 *   frontend bundle or served as a static file.
 * - If no token is set, this function returns `null` every time it is called.
 *   Callers must read it per request so that a token set after the app loads
 *   is picked up immediately.
 */
export function getApiToken(): string | null {
  if (typeof localStorage === 'undefined') {
    return null;
  }
  return localStorage.getItem(LS_TOKEN_KEY);
}

/**
 * Persist an API token to localStorage and use it for subsequent requests.
 */
export function setApiToken(token: string): void {
  if (typeof localStorage !== 'undefined') {
    localStorage.setItem(LS_TOKEN_KEY, token);
  }
}

/**
 * Remove the persisted API token from localStorage.
 */
export function clearApiToken(): void {
  if (typeof localStorage !== 'undefined') {
    localStorage.removeItem(LS_TOKEN_KEY);
  }
}

async function request<T>(path: string, options?: RequestInit): Promise<T> {
  const headers: Record<string, string> = {};

  if (options?.method && ['POST', 'PUT', 'PATCH'].includes(options.method)) {
    headers['Content-Type'] = 'application/json';
  }

  if (options?.headers) {
    const optsHeaders = options.headers as Record<string, string>;
    Object.assign(headers, optsHeaders);
  }

  const token = getApiToken();
  if (token) {
    headers['Authorization'] = `Bearer ${token}`;
  }

  const resp = await fetch(`${BASE_URL}${path}`, {
    ...options,
    headers,
  });

  if (!resp.ok) {
    const text = await resp.text().catch(() => '');
    // Parse the machine-readable `code` from JSON error bodies so callers can
    // branch on it (e.g. `auth_required`) without parsing the message text.
    let code: string | undefined;
    if (text) {
      try {
        const parsed = JSON.parse(text) as { code?: unknown };
        if (typeof parsed.code === 'string') code = parsed.code;
      } catch {
        // Non-JSON body — keep the message-text fallback below.
      }
    }
    const err = new Error(`HTTP ${resp.status}: ${resp.statusText}${text ? ' — ' + text : ''}`) as ApiError;
    // Attach the numeric status so callers can branch on 4xx/5xx instead of
    // parsing the message text (e.g. rerun's 404/409/422 handling).
    err.status = resp.status;
    err.code = code;
    // Surface auth signals (bootstrap needed / invalid token) so the app can
    // switch to the right screen instead of only showing an error toast.
    if (resp.status === 401 && code) {
      dispatchAuthSignal(code);
    }
    throw err;
  }

  const contentType = resp.headers.get('content-type') || '';
  if (contentType.includes('application/json')) {
    return resp.json() as Promise<T>;
  }

  return undefined as unknown as T;
}

export { request };
