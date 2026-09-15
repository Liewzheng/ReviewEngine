import type { LlmProviderStatus } from '../../types/llm'
import { duplicateCard as cloneCard, type ProviderCardState } from '../../composables/llmPayload'

/** The runtime health entry a card is rendered from (`GET /llm/providers`). */
export interface CardHealth {
  name: string
  status: LlmProviderStatus
  disabled: boolean
  apiBaseUrl?: string
  defaultModel?: string
  position?: number
  chainPosition?: number | null
  isPrimary?: boolean
  avgLatencyMs?: number | null
  requestCount?: number | null
  successRate?: number | null
  usageShare?: number | null
}

/** One tooltip/label pair on the stats row, in render order. */
export interface StatCell {
  key: 'avgLatency' | 'requests' | 'successRate'
  labelKey: string
  text: string
  empty: boolean
}

/** What the reserved 32px footer slot shows for a card. */
export type CardFooterKind = 'none' | 'disabled' | 'offline' | 'error' | 'testOk' | 'testFail'

export interface CardFooter {
  kind: CardFooterKind
  /** Failure reason / test error text. Empty for the steady `none` footer. */
  detail: string
}

/**
 * The visual state a card is in, as a single value.
 *
 * `disabled` wins over the health status: a switched-off card is not probed,
 * so its reported status is `disabled` as well — but a card can also be
 * flagged by the config echo before the health list arrives.
 */
export function cardVisualState(card: ProviderCardState, health?: CardHealth): LlmProviderStatus {
  if (card.disabled || health?.disabled || health?.status === 'disabled') return 'disabled'
  return health?.status ?? 'offline'
}

/** The left-edge accent stripe a card carries (RENG-75): a coloured stripe
 *  reads as "this card needs attention", a healthy one stays undecorated. */
export function cardAccent(card: ProviderCardState, health?: CardHealth): 'error' | 'degraded' | 'none' {
  const state = cardVisualState(card, health)
  if (state === 'error') return 'error'
  if (state === 'degraded') return 'degraded'
  return 'none'
}

/**
 * i18n key of the status pill's label. `disabled` is its own state: the
 * payload reports it instead of `offline` so the card can say "switched off"
 * rather than implying an unreachable provider.
 */
export function statusLabelKey(card: ProviderCardState, health?: CardHealth): string {
  return `llm.status.${cardVisualState(card, health)}`
}

/**
 * The footer slot's content.
 *
 * A manual connectivity test (RENG-54) outranks the steady state while its
 * result is in this page's session — the user just asked for that
 * measurement, and the card face has no button left to repeat it.
 */
export function cardFooter(
  card: ProviderCardState,
  health: CardHealth | undefined,
  probeMessage: string | null,
  testResult?: { success: boolean; latencyMs?: number; error?: string } | null,
): CardFooter {
  if (testResult) {
    return testResult.success
      ? { kind: 'testOk', detail: '' }
      : { kind: 'testFail', detail: testResult.error ?? '' }
  }
  const state = cardVisualState(card, health)
  if (state === 'disabled') return { kind: 'disabled', detail: '' }
  if (state === 'error') return { kind: 'error', detail: probeMessage ?? '' }
  if (state === 'offline') return { kind: 'offline', detail: '' }
  return { kind: 'none', detail: '' }
}

/** Monogram for the avatar: up to two alphanumerics of the display name. */
export function monogramOf(name: string): string {
  const cleaned = name.replace(/[^A-Za-z0-9]/g, '')
  return cleaned.slice(0, 2).toUpperCase() || '?'
}

/** `—` for a value the server did not report: unknown, never `0`. */
export const EM_DASH = '—'

/** Mean recorded call latency over the window. */
export function formatLatency(ms: number | null | undefined): string {
  if (ms === null || ms === undefined) return '—'
  return `${ms}ms`
}

/** Recorded usages (review-level count) over the window. */
export function formatRequests(count: number | null | undefined): string {
  if (count === null || count === undefined) return '—'
  return new Intl.NumberFormat('en-US').format(count)
}

/** Success rate as a fraction (0..1) rendered as a percentage. */
export function formatSuccessRate(rate: number | null | undefined): string {
  if (rate === null || rate === undefined) return '—'
  return `${(rate * 100).toFixed(1)}%`
}

/** Usage share as a fraction (0..1) → one-decimal percentage, or `null` when
 *  the server has no usage history at all (the bar and its label hide). */
export function formatUsagePercent(share: number | null | undefined): number | null {
  if (share === null || share === undefined) return null
  return Math.round(share * 1000) / 10
}

/** The three stats values with their label-only tooltips, in render order. */
export function statsRow(health?: CardHealth): StatCell[] {
  const latency = formatLatency(health?.avgLatencyMs)
  const requests = formatRequests(health?.requestCount)
  const successRate = formatSuccessRate(health?.successRate)
  return [
    { key: 'avgLatency', labelKey: 'llm.metrics.avgLatency', text: latency, empty: latency === EM_DASH },
    { key: 'requests', labelKey: 'llm.metrics.requests', text: requests, empty: requests === EM_DASH },
    {
      key: 'successRate',
      labelKey: 'llm.metrics.successRate',
      text: successRate,
      empty: successRate === EM_DASH,
    },
  ]
}

/** `true` when the card's stats row has nothing measured to show. */
export function statsAreEmpty(stats: StatCell[]): boolean {
  return stats.every((s) => s.empty)
}

/** Identity of a card for the runtime list: the visible
 *  `(provider, apiBaseUrl, model)` triple. */
export function healthTriple(entry: { name: string; apiBaseUrl?: string; defaultModel?: string }): string {
  return [entry.name, entry.apiBaseUrl ?? '', entry.defaultModel ?? ''].join('\u0000')
}

/** Identity of a config card, spelled the same way. */
export function cardTriple(card: ProviderCardState): string {
  return [card.provider, card.apiBaseUrl, card.defaultModel].join('\u0000')
}

/**
 * Align the runtime health list with the config cards.
 *
 * RENG-75 made the provider NAME a display label, so the page can no longer
 * join the two lists by name: two cards may legitimately share one, and
 * joining by name would show the first account's latency on the second card.
 * The join is the visible triple — the same one `PUT /config`'s masked-keep
 * resolution walks — with the stored position as the fallback for a payload
 * that predates `apiBaseUrl`/`defaultModel` or for two cards whose triples
 * are identical (their order is the only thing telling them apart).
 */
export function matchHealthToCards<T extends CardHealth>(
  cards: ProviderCardState[],
  providers: T[],
): (T | undefined)[] {
  const byTriple = new Map<string, T[]>()
  for (const p of providers) {
    const key = healthTriple(p)
    const bucket = byTriple.get(key)
    if (bucket) bucket.push(p)
    else byTriple.set(key, [p])
  }
  return cards.map((card, index) => {
    const matches = byTriple.get(cardTriple(card))
    if (matches && matches.length === 1) return matches[0]
    const byPosition = providers[index]
    if (byPosition && byPosition.name === card.provider) return byPosition
    if (matches && matches.length > 1) return matches[0]
    return undefined
  })
}

/**
 * Index the `/system/health` LLM rows by the card they describe.
 *
 * `GET /llm/providers` deliberately carries no failure text (only the probe's
 * status and timing), while `/system/health` reports the probe's message
 * ("HTTP 401 Unauthorized"); it labels a row `"<provider> <model>"`. Keyed by
 * the triple so two same-named cards cannot share one message.
 */
export function matchProbeMessages<T extends { service: string; message?: string }>(
  cards: ProviderCardState[],
  healthRows: T[],
): (string | null)[] {
  const byLabel = new Map<string, string>()
  for (const row of healthRows) {
    const message = row.message?.trim()
    if (message) byLabel.set(row.service, message)
  }
  return cards.map((card) => byLabel.get(`${card.provider} ${card.defaultModel}`) ?? null)
}

/** What the add/edit dialog is opened with, and what a save then addresses. */
export interface ProviderDialogOpen {
  mode: 'add' | 'edit'
  /** Card the form starts from. */
  initial: ProviderCardState
  /**
   * Grid index the save replaces, or `-1` when the save APPENDS a new card
   * (add and duplicate both append).
   */
  index: number
}

/**
 * "编辑" — open the form on a card, saving back over the same grid position.
 * The card is addressed by INDEX, never by name: two cards may share one.
 */
export function dialogForEdit(
  cards: ProviderCardState[],
  index: number,
): ProviderDialogOpen | null {
  const card = cards[index]
  if (!card) return null
  return { mode: 'edit', initial: { ...card }, index }
}

/**
 * "复制卡片" — open the ADD form pre-filled with this card and a copy of its
 * API key, so the copy starts life as a real sibling. The key travels as the
 * `***` sentinel the echo carries (the secret never reaches the browser); the
 * server's masked-keep resolution turns it back into the original entry's
 * stored key while the triple still matches it, and an edit to the copy's
 * base URL or model is what makes it a distinct entry.
 */
export function dialogForDuplicate(
  cards: ProviderCardState[],
  index: number,
): ProviderDialogOpen | null {
  const card = cards[index]
  if (!card) return null
  return { mode: 'add', initial: cloneCard(card), index: -1 }
}
