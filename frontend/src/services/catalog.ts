import { request } from './api';
import type { CatalogModelsResponse, CatalogProvidersResponse } from '../types/catalog';

/**
 * Fetch the models.dev provider catalog.
 *
 * Only providers that expose a base API URL are included, sorted by name.
 * Throws an ApiError (status 503) when the catalog is unavailable — callers
 * must handle that gracefully by falling back to the built-in preset list.
 */
export async function fetchCatalogProviders(): Promise<CatalogProvidersResponse> {
  return request('/catalog/providers');
}

/**
 * Fetch the catalog's model list for one provider, sorted by name.
 * Throws an ApiError on unknown provider (404) or catalog outage (503).
 * @param providerId - Catalog provider id (e.g. `deepseek`).
 */
export async function fetchCatalogModels(providerId: string): Promise<CatalogModelsResponse> {
  return request(`/catalog/providers/${encodeURIComponent(providerId)}/models`);
}
