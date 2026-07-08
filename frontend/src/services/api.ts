const BASE_URL = '/api/v1';
const LS_TOKEN_KEY = 'review_engine_api_token';

let apiTokenPromise: Promise<string | null> | null = null;

async function fetchApiToken(): Promise<string | null> {
  if (typeof localStorage !== 'undefined') {
    const lsToken = localStorage.getItem(LS_TOKEN_KEY);
    if (lsToken) {
      return lsToken;
    }
  }

  try {
    const resp = await fetch('/config.json');
    if (!resp.ok) {
      return null;
    }
    const config = (await resp.json()) as { apiToken?: unknown };
    if (config.apiToken && typeof config.apiToken === 'string') {
      return config.apiToken;
    }
  } catch {
    // Ignore: /config.json is optional in dev and may not exist.
  }

  return null;
}

export async function getApiToken(): Promise<string | null> {
  if (!apiTokenPromise) {
    apiTokenPromise = fetchApiToken();
  }
  return apiTokenPromise;
}

export function setApiToken(token: string): void {
  apiTokenPromise = Promise.resolve(token);
  if (typeof localStorage !== 'undefined') {
    localStorage.setItem(LS_TOKEN_KEY, token);
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

  const token = await getApiToken();
  if (token) {
    headers['Authorization'] = `Bearer ${token}`;
  }

  const resp = await fetch(`${BASE_URL}${path}`, {
    ...options,
    headers,
  });

  if (!resp.ok) {
    const text = await resp.text().catch(() => '');
    throw new Error(`HTTP ${resp.status}: ${resp.statusText}${text ? ' — ' + text : ''}`);
  }

  const contentType = resp.headers.get('content-type') || '';
  if (contentType.includes('application/json')) {
    return resp.json() as Promise<T>;
  }

  return undefined as unknown as T;
}

export { request };
