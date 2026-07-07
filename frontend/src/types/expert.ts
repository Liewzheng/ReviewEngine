export type ExpertCategory = 'security' | 'performance' | 'quality' | 'maintainability' | 'test-coverage' | 'documentation' | 'dependencies' | 'accessibility' | 'architecture'

export interface ExpertReviewSummary {
  reviewId: string
  mrTitle: string
  score?: number
  date: string
}

export interface Expert {
  id: string
  name: string
  category: ExpertCategory
  icon: string
  enabled: boolean
  weight: number
  description: string
  promptPreview: string
  lastReviews: ExpertReviewSummary[]
}

export const categoryColorMap: Record<ExpertCategory, string> = {
  security: '#ef4444',
  performance: '#f59e0b',
  quality: '#22c55e',
  maintainability: '#3b82f6',
  'test-coverage': '#8b5cf6',
  documentation: '#06b6d4',
  dependencies: '#ec4899',
  accessibility: '#10b981',
  architecture: '#6366f1',
}

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
