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

/** Maps each expert category to its display color (hex). */
export const categoryColorMap: Record<ExpertCategory, string> = {
  security: '#ef4444',
  performance: '#f59e0b',
  quality: '#22c55e',
  maintainability: '#3b82f6',
  'test-coverage': '#a855f7',
  documentation: '#6b7280',
  dependencies: '#6366f1',
  accessibility: '#ec4899',
  architecture: '#14b8a6',
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
