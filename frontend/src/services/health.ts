import { request } from './api';
import type { SystemHealth } from '../types/dashboard';

/**
 * Fetch the server's system health status.
 * @returns System health information (uptime, memory, version, etc.).
 */
export async function getSystemHealth(): Promise<SystemHealth> {
  return request('/system/health');
}
