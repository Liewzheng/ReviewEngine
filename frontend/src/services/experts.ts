import { request } from './api';
import type { Expert } from '../types/expert';

export async function getExperts(): Promise<{ experts: Expert[] }> {
  return request('/system/experts');
}

export async function updateExpert(
  id: string,
  data: { enabled?: boolean; weight?: number }
): Promise<Expert> {
  return request(`/system/experts/${id}`, {
    method: 'PUT',
    body: JSON.stringify(data),
  });
}
