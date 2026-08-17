import { request } from './api';
import type { Expert } from '../types/expert';

/**
 * Fetch all expert definitions from the server.
 * @returns Object containing the `experts` array.
 */
export async function getExperts(): Promise<{ experts: Expert[] }> {
  return request('/system/experts');
}

/**
 * Update a single expert's configuration.
 * @param id - Expert identifier.
 * @param data - Fields to update (enabled, weight).
 * @returns The updated expert definition.
 */
export async function updateExpert(
  id: string,
  data: { enabled?: boolean; weight?: number }
): Promise<Expert> {
  return request(`/system/experts/${id}`, {
    method: 'PUT',
    body: JSON.stringify(data),
  });
}
