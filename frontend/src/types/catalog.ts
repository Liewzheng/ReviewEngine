/** A provider entry from the models.dev catalog (only providers exposing a base API URL). */
export interface CatalogProvider {
  /** Provider identifier (e.g. `deepseek`). */
  id: string;
  /** Display name (e.g. `DeepSeek`). */
  name: string;
  /** Base API URL exposed by the provider. */
  api_base: string;
  /** Credential environment variables; the first entry is the API key variable. */
  env: string[];
  /** Optional documentation URL. */
  doc?: string;
  /** Number of models the catalog lists for this provider. */
  model_count: number;
}

/** Response shape of GET /catalog/providers. */
export interface CatalogProvidersResponse {
  /** Catalog providers sorted by name. */
  providers: CatalogProvider[];
}

/** A model entry from the models.dev catalog. */
export interface CatalogModel {
  /** Model identifier (e.g. `deepseek-chat`). */
  id: string;
  /** Display name (e.g. `DeepSeek Chat`). */
  name: string;
  /** Context window size in tokens (null when the catalog does not know). */
  context_limit: number | null;
  /** Maximum output tokens (null when the catalog does not know). */
  output_limit: number | null;
  /** Whether the model supports reasoning (null when unknown). */
  reasoning: boolean | null;
  /** Whether the model supports tool calls (null when unknown). */
  tool_call: boolean | null;
}

/** Response shape of GET /catalog/providers/{id}/models. */
export interface CatalogModelsResponse {
  /** Catalog models sorted by name. */
  models: CatalogModel[];
}
