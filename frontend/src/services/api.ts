const BASE_URL = '/api/v1';

async function request<T>(path: string, options?: RequestInit): Promise<T> {
  const headers: Record<string, string> = {};

  if (options?.method && ['POST', 'PUT', 'PATCH'].includes(options.method)) {
    headers['Content-Type'] = 'application/json';
  }

  if (options?.headers) {
    const optsHeaders = options.headers as Record<string, string>;
    Object.assign(headers, optsHeaders);
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
