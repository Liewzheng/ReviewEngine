import { request } from './api';
import type { StorageBackendKind, SystemHealth } from '../types/dashboard';

const STORAGE_BACKENDS: readonly StorageBackendKind[] = ['postgresql', 'sqlite', 'disabled'];

/**
 * Fetch the server's system health status.
 * @returns System health information (uptime, memory, version, etc.).
 */
export async function getSystemHealth(): Promise<SystemHealth> {
  // `storage_backend` (0.10.0) is the one snake_case key on this otherwise
  // camelCase payload; normalize it here so consumers see `storageBackend`.
  // Unknown/absent values degrade to undefined (the caller hides the row).
  const raw = await request<SystemHealth & { storage_backend?: string }>('/system/health');
  return {
    ...raw,
    storageBackend: STORAGE_BACKENDS.find((k) => k === raw.storage_backend),
  };
}
