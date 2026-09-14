import { ref } from 'vue'
import { getReviews } from '../services/reviews'
import type { LlmUsage, ReviewListItem } from '../types/history'
import { i18n } from '../i18n'

/**
 * "What did the last reviews actually run on?" for the LLM Status page
 * (RENG-55).
 *
 * The provider that answered is what `reviews.llm_summary` records per review
 * (RENG-38), so the LLM page reads the review list it already has an endpoint
 * for instead of growing a stats endpoint (per-request counts and latency
 * belong to the separate metrics work, RENG-56/57).
 */

/** How many recent reviews the LLM page inspects. */
export const RECENT_USAGE_LIMIT = 8

/** One rendered row: the review's LLMs plus whether it skipped the chain head. */
export interface RecentLlmUsageEntry {
  /** Review task id — the list key. */
  id: string
  /** When the review was created (ISO 8601), for the row's timestamp. */
  createdAt: string
  /** Deduplicated `provider/model` pairs that produced the review. */
  usages: LlmUsage[]
  /**
   * True when NONE of this review's usages is the current chain head — i.e.
   * the review ran entirely on a fallback provider. Reviews recorded before
   * the chain was configured (`headProvider` unknown) are never marked.
   */
  primaryUnused: boolean
}

/** `created_at` DESC items → the usage rows, newest first. Pure. */
export function deriveRecentUsage(items: ReviewListItem[], headProvider: string): RecentLlmUsageEntry[] {
  return items
    .filter((item) => (item.llmSummary?.length ?? 0) > 0)
    .map((item) => {
      const usages = item.llmSummary ?? []
      return {
        id: item.id,
        createdAt: item.createdAt,
        usages,
        primaryUnused: !!headProvider && usages.every((u) => u.provider !== headProvider),
      }
    })
}

export function useRecentLlmUsage(limit: number = RECENT_USAGE_LIMIT) {
  const entries = ref<RecentLlmUsageEntry[]>([])
  const loading = ref(false)
  const error = ref<string | null>(null)

  /**
   * Load the newest `limit` reviews and keep the ones that carry an LLM
   * snapshot. `headProvider` is the chain head the marker compares against;
   * an empty string (nothing configured / runtime list unavailable) disables
   * the marker but still lists what ran.
   *
   * Fail-soft: the section is informational, so an error clears the list and
   * a broken reviews read never takes the provider cards down with it.
   */
  async function load(headProvider: string) {
    loading.value = true
    try {
      const response = await getReviews(
        { q: '', project: null, status: null, dateFrom: null, dateTo: null, repository: null },
        1,
        limit
      )
      entries.value = deriveRecentUsage(response.items, headProvider)
      error.value = null
    } catch (e) {
      error.value = e instanceof Error ? e.message : i18n.global.t('errors.unknown')
      entries.value = []
    } finally {
      loading.value = false
    }
  }

  return { entries, loading, error, load }
}
