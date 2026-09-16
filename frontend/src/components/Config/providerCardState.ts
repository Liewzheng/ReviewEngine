import type { LlmProviderStatus } from '../../types/llm'
import {
  createEmptyProviderCard,
  duplicateCard as cloneCard,
  type ProviderCardState,
} from '../../composables/llmPayload'

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
  /**
   * RENG-78: mean round-trip time of the probes over the window, in whole
   * milliseconds — the card's "communication latency", a pure network
   * measurement (one `GET {api_base}/models`, no model involved). `null` when
   * no probe succeeded in the window; optional so a payload written before the
   * field shipped falls back to `avgLatencyMs` instead of showing nothing.
   */
  avgProbeLatencyMs?: number | null
  /**
   * RENG-78: successful probes behind `avgProbeLatencyMs` — the KPI's weight
   * for a provider whose reading came from the probe.
   */
  probeSampleCount?: number | null
  /**
   * RENG-57: successful calls behind `avgLatencyMs`. Not rendered on the card
   * (which shows a duration, not a count); it is the KPI strip's weight for a
   * provider whose reading fell back to the recorded call latency.
   */
  latencySampleCount?: number | null
  /**
   * RENG-77: mean communication latency (time to first byte) of the calls the
   * server recorded for this provider, in whole milliseconds; `null` when no
   * sample carries a TTFB yet. RENG-78: kept on the payload, no longer the
   * card's primary display (a non-streaming provider reports `ttfb ≈ latency`,
   * so it is not the network metric this card wants).
   */
  avgTtfbMs?: number | null
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
  /** The measurement behind `text` exactly as the server reported it, for the
   *  hover tooltip — the face shows a rounded single token. */
  exact?: string
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

/** `—` for a value the server did not report: unknown, never `0`. */
export const EM_DASH = '—'

/**
 * A measured duration as ONE token, so a KPI value or a stat cell can never
 * wrap onto a second line: whole milliseconds below a second, one-decimal
 * seconds below a minute, one-decimal minutes above it. The unrounded value
 * stays available through {@link StatCell.exact}.
 */
export function formatLatency(ms: number | null | undefined): string {
  if (ms === null || ms === undefined) return EM_DASH
  if (ms < 1000) return `${Math.round(ms)}ms`
  if (ms < 60_000) return `${(ms / 1000).toFixed(1)}s`
  return `${(ms / 60_000).toFixed(1)}min`
}

/** A measurement as the server reported it, for a tooltip. */
export function exactMs(ms: number | null | undefined): string | undefined {
  return ms === null || ms === undefined ? undefined : `${ms} ms`
}

/** Which measurement a latency reading came from. `probe` is the probe's own
 *  round trip — pure network latency; `latency` the recorded LLM call (which
 *  contains generation, so it is the fallback). */
export type LatencySource = 'probe' | 'latency'

export interface LatencyReading {
  ms: number | null
  /** `null` when neither measurement exists — the reading is `—`. */
  source: LatencySource | null
}

/**
 * The latency a card (or the KPI strip) shows.
 *
 * RENG-78: the communication latency (`avgProbeLatencyMs`) comes first — it is
 * the round trip of the lightweight probe, i.e. the network and nothing else,
 * which is what the number is meant to say. The recorded call latency
 * (`avgLatencyMs`) is the fallback for a card whose probes have not been
 * sampled yet (the field is `null`, or absent in a payload written before it
 * shipped). The two are different measurements, so the reading carries which
 * one it is and the UI labels it accordingly — and the label never mentions a
 * probe: the user is reading a duration, not a mechanism.
 */
export function latencyReading(health?: CardHealth): LatencyReading {
  const probe = health?.avgProbeLatencyMs
  if (typeof probe === 'number') return { ms: probe, source: 'probe' }
  const call = health?.avgLatencyMs
  if (typeof call === 'number') return { ms: call, source: 'latency' }
  return { ms: null, source: null }
}

/** i18n key naming the measurement a reading carries. */
export function latencyLabelKey(source: LatencySource | null): string {
  return source === 'probe' ? 'llm.metrics.avgCommLatency' : 'llm.metrics.avgLatency'
}

/**
 * The KPI strip's number: the weighted mean of what the cards show.
 *
 * Each provider contributes the reading its own card shows, weighted by the
 * sample count behind THAT reading — a provider with 210 probes is not the
 * same evidence as one with 2, and using the other measurement's count would
 * weight the wrong number. The label follows the same rule as the mean: the
 * strip says "communication latency" as soon as one contributing provider's
 * reading came from a probe, and "latency" only when every one of them fell
 * back to the recorded call latency — one number, never two described as one.
 */
export function stripLatency(providers: CardHealth[]): LatencyReading {
  const measured = providers
    .map((p) => {
      const reading = latencyReading(p)
      const weight = reading.source === 'probe' ? (p.probeSampleCount ?? 0) : (p.latencySampleCount ?? 0)
      return { reading, weight }
    })
    .filter((r) => r.reading.ms !== null && r.weight > 0)
  if (!measured.length) return { ms: null, source: null }
  const total = measured.reduce((sum, r) => sum + r.weight, 0)
  const weighted = measured.reduce((sum, r) => sum + (r.reading.ms as number) * r.weight, 0)
  return {
    ms: Math.round(weighted / total),
    source: measured.some((r) => r.reading.source === 'probe') ? 'probe' : 'latency',
  }
}

/** The KPI strip's `llm.stats` labels for a reading. */
export interface StripLatencyLabelKeys {
  /** The measurement, without its window. */
  label: string
  /** The same measurement named over the window the samples cover. */
  window: string
}

/**
 * The i18n keys the strip labels its number with, for the reading it took.
 *
 * RENG-87: the pair is listed, never composed. The strip used to ask for
 * `<key>Window` by concatenation, and for a probe reading that produced
 * `llm.stats.avgCommLatencyWindow` — a key no locale carried, which vue-i18n
 * renders as the key itself (`llm.stats.avgC…` on screen). Both keys are now
 * named here, next to the reading that picks them, and pinned against every
 * locale in `providerCardState.spec.ts`.
 */
export function stripLatencyLabelKeys(source: LatencySource | null): StripLatencyLabelKeys {
  return source === 'probe'
    ? { label: 'llm.stats.avgCommLatency', window: 'llm.stats.avgCommLatencyWindow' }
    : { label: 'llm.stats.avgLatency', window: 'llm.stats.avgLatencyWindow' }
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
  const reading = latencyReading(health)
  const latency = formatLatency(reading.ms)
  const requests = formatRequests(health?.requestCount)
  const successRate = formatSuccessRate(health?.successRate)
  return [
    {
      key: 'avgLatency',
      labelKey: latencyLabelKey(reading.source),
      text: latency,
      empty: latency === EM_DASH,
      exact: exactMs(reading.ms),
    },
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

/**
 * The three ways the provider dialog opens.
 *
 * `add` starts from a blank card; `edit` from the card at `index` and saves
 * back over it; `duplicate` (RENG-83) from a copy of the card at `index` and
 * saves as a new card appended to the grid.
 */
export type ProviderDialogMode = 'add' | 'edit' | 'duplicate'

/** What the add/edit dialog is opened with, and what a save then addresses. */
export interface ProviderDialogOpen {
  mode: ProviderDialogMode
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
 * "复制卡片" — open the form pre-filled with a copy of this card, so the copy
 * starts life as a real sibling and the user only has to change the field that
 * makes it different (RENG-83: the form used to open blank, which made the
 * action pointless). The `'duplicate'` mode is what lets the dialog tell this
 * apart from a plain 「添加供应商」, which must keep opening empty.
 *
 * The API key travels as the `***` sentinel the echo carries (the secret never
 * reaches the browser); the server's masked-keep resolution turns it back into
 * the original card's stored key while the triple still matches it. The dialog
 * does not put the sentinel in the key input — it opens that field blank with
 * the "leave empty to keep the saved key" placeholder, the shape edit mode has
 * always had — see {@link initialDialogForm}.
 */
export function dialogForDuplicate(
  cards: ProviderCardState[],
  index: number,
): ProviderDialogOpen | null {
  const card = cards[index]
  if (!card) return null
  return { mode: 'duplicate', initial: cloneCard(card), index: -1 }
}

/**
 * The form a dialog opens with (RENG-83).
 *
 * `edit` and `duplicate` both start from `initial`; the API key starts BLANK in
 * both, because the browser never holds the secret. `GET /config` echoes the
 * `***` mask instead, and the server resolves a blank (or masked) key back to a
 * stored key by the `(provider, api_base, model)` triple — so an empty field is
 * the accurate "keep the saved key", while the sentinel in a text input would
 * read as a key the user typed. A duplicate edited into a DIFFERENT triple no
 * longer matches a stored card, so the key is not kept (the server keeps
 * nothing rather than guessing between two same-triple accounts); that is the
 * same rule edit mode has always followed.
 *
 * `add` is the one mode that ignores `initial`: the 「添加供应商」 button can
 * never inherit a card, however a caller passes one.
 *
 * Optional fields are filled from the empty card first, so a card that carries
 * no `disabled`/`disableThinking` cannot leave a previous dialog session's
 * switch state behind.
 */
export function initialDialogForm(
  mode: ProviderDialogMode,
  initial?: ProviderCardState | null,
): ProviderCardState {
  if (mode === 'add' || !initial) return createEmptyProviderCard()
  return { ...createEmptyProviderCard(), ...initial, apiKey: '' }
}

/** i18n key of the "nothing was changed" prompt a duplicate save raises. */
export const DUPLICATE_UNCHANGED_KEY = 'config.providerCards.duplicateUnchanged'

/**
 * True when a duplicate dialog's form still holds exactly the values it was
 * opened with — the user copied a card and pressed save without touching it,
 * which adds a second card identical to the first.
 *
 * This feeds a PROMPT, never a silent block or a silent refusal: the client's
 * view of a card's identity stops at the `(provider, api_base, model)` triple,
 * while RENG-75's real identity includes the API key it cannot see. Two
 * same-triple cards are legitimate (one account, two keys), so the user gets
 * the last word rather than the client deciding for them.
 */
export function duplicateUnchanged(
  snapshot: ProviderCardState,
  form: ProviderCardState,
): boolean {
  return cardSignature(snapshot) === cardSignature(form)
}

/**
 * Every field of a card, in a fixed order, as one comparable string.
 *
 * Strings are compared TRIMMED because that is what a save submits — a stray
 * space is not a modification. The optional booleans compare as `false` when
 * absent, so a payload written before `disableThinking` shipped and one that
 * carries an explicit `false` describe the same card.
 */
function cardSignature(card: ProviderCardState): string {
  return [
    card.provider.trim(),
    card.apiKey.trim(),
    card.apiBaseUrl.trim(),
    card.defaultModel.trim(),
    card.maxTokens,
    card.temperature,
    card.timeoutSeconds,
    card.retryAttempts,
    card.disabled ?? false,
    card.disableThinking ?? false,
  ].join('\u0000')
}
