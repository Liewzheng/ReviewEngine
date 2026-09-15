/** Expert review category classification. */
export type ExpertCategory = 'security' | 'performance' | 'quality' | 'maintainability' | 'test-coverage' | 'documentation' | 'dependencies' | 'accessibility' | 'architecture'

/** Summary of a past review performed by an expert. */
export interface ExpertReviewSummary {
  /** Review identifier. */
  reviewId: string
  /** MR/PR title that was reviewed. */
  mrTitle: string
  /** Score assigned by this expert (0–100). */
  score?: number
  /** ISO 8601 date when the review was performed. */
  date: string
}

/** An AI review expert definition with its configuration and history. */
export interface Expert {
  /** Unique expert identifier. */
  id: string
  /** Expert display name. */
  name: string
  /** Review category this expert specializes in. */
  category: ExpertCategory
  /** Icon identifier or URL for the expert card. */
  icon: string
  /** Whether this expert is enabled for reviews. */
  enabled: boolean
  /** Weight factor for the expert's score in the overall rating. */
  weight: number
  /** Human-readable description of what this expert reviews. */
  description: string
  /** Preview of the expert's LLM prompt (truncated). */
  promptPreview: string
  /** Recent reviews performed by this expert. */
  lastReviews: ExpertReviewSummary[]
}

/**
 * Response of `PUT /system/experts/{id}`: the expert as the server now holds
 * it, plus whether that state is durable.
 *
 * `persisted === false` means the server has no configuration store attached
 * (`REVIEW_DISABLE_DB=1`, embedded use): the edit took effect in memory but is
 * LOST when the server restarts. The page must say so instead of showing a
 * plain success.
 */
export interface ExpertUpdateResult extends Expert {
  persisted?: boolean
}

/** Maps each expert category to its human-readable label. */
export const categoryLabelMap: Record<ExpertCategory, string> = {
  security: 'Security',
  performance: 'Performance',
  quality: 'Quality',
  maintainability: 'Maintainability',
  'test-coverage': 'Test Coverage',
  documentation: 'Documentation',
  dependencies: 'Dependencies',
  accessibility: 'Accessibility',
  architecture: 'Architecture',
}
