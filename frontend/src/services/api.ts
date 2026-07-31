const BASE_URL = '/api/v1';
const LS_TOKEN_KEY = 'review_engine_api_token';

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
    const err = new Error(`HTTP ${resp.status}: ${resp.statusText}${text ? ' — ' + text : ''}`) as Error & { status?: number };
    // Attach the numeric status so callers can branch on 4xx/5xx instead of
    // parsing the message text (e.g. rerun's 404/409/422 handling).
    err.status = resp.status;
    throw err;
  }

  const contentType = resp.headers.get('content-type') || '';
  if (contentType.includes('application/json')) {
    return resp.json() as Promise<T>;
  }

  return undefined as unknown as T;
}

export { request };
