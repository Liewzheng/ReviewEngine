import { request } from './api';
import type { SystemHealth } from '../types/dashboard';

export async function getSystemHealth(): Promise<SystemHealth> {
  return request('/system/health');
}
