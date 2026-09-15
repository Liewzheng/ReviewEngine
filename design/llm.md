# LLM Status Page Design

## 1. Route & Purpose

- **Route**: `/llm`
- **Purpose**: Monitor the health and performance of each LLM provider. Quick diagnostic view for troubleshooting review failures or latency spikes.
- **Data**: Fetched on mount, then refreshed by the page's 30 s silent auto-refresh (no manual refresh button, RENG-52; there is no SSE feed for this page).

## 2. Page Layout

```
PageHeader
├── Title: "LLM Status" + subtitle: "Provider health and performance"
└── Right: Add provider button (ElButton icon: Plus) — the page auto-refreshes every 30 s, no manual refresh (RENG-52)

Provider Grid
├── ProviderCard (OpenAI)   [Primary] [Chain #1]
├── ProviderCard (Anthropic)           [Chain #2]
├── ProviderCard (Ollama Local)        [Chain #3]
└── ProviderCard (Additional...)

Recent Usage card (RENG-55)
└── one row per recent review: time · provider/model [Primary not used]
```

### 2.1 Chain order and effective-provider visibility (RENG-55)

The cards are the *configuration*; the runtime order they are tried in is the
**authoritative chain** — the persisted primary first, then the remaining
providers in their stored order (`ordered_llm_configs` in `src/llm/mod.rs`).
The page must therefore make three things visible, all of them derived from
existing payloads (no new data source):

- **Primary** — `primaryBadge` on the card whose provider is
  `llm.primaryProvider` (config echo, as before).
- **Chain rank** — a quiet `Chain #N` / `链序 #N` tag per card from
  `chainPosition` of `GET /llm/providers` (1-based; the head is 1). `position`
  in the same payload stays the stored index, which is what the card order
  encodes. The grid keeps the stored order — the marker, not a re-sort,
  expresses the chain.
- **Who may move the primary (RENG-72)** — only a save that carries the
  user's choice: **Set as Primary**, the first card of an empty page, or the
  deletion of the last provider. An add/edit omits `llm.primaryProvider`
  from its `PUT {llm}` so the backend keeps the stored selection, and the
  primary card cannot be deleted while another provider remains (the
  successor would be the array head, not a choice) — the alert says so and
  the card stays. The chain head the runtime uses is therefore always a
  selection someone made.
- **What actually ran** — the Recent Usage card lists the newest reviews'
  `reviews.llm_summary` `provider/model` pairs (via the existing
  `GET /api/v1/reviews` list endpoint, 8 rows). A row whose usages contain
  none of the chain head's provider is tagged `未使用首选` / Primary not used,
  which is how a fallback that skipped the primary becomes visible instead of
  looking like a normal run.

### 2.2 Recorded usage statistics (RENG-56)

Each card's usage numbers are the reviews that provider actually served, not
placeholders: `GET /api/v1/llm/providers` aggregates `reviews.llm_summary`
over a rolling window (7 days; the payload carries `usageWindowDays` /
`usageSince` and the UI labels the numbers with them). Four real values per
card — `requestCount` (reviews that used the provider), `usageShare` (its
slice of all usage in the window), `successRate` (over the reviews that used
it and finished) and `lastUsedAt` — plus the recorded call latency of §2.3.

Anything without a basis is `null` on the wire and `—` on the page: there is
no capacity concept (`usagePercent` is gone) and no fabricated series. The card
shows `0` only when the window was really read and really held no usage.

### 2.3 Recorded call latency (RENG-57)

The card's latency number is the mean round-trip time of the **calls the
reviews actually made**, not the probe's instantaneous value: every LLM call
attempt on the review path records a row in `llm_call_samples` (timestamp,
provider, model, `latency_ms`, success/failure + error, chain position,
attempt, review id), and `GET /api/v1/llm/providers` folds the window's rows
into `avgLatencyMs` / `latencySampleCount` / `latencyFailureCount` /
`latencyLastSampleAt` / `latencySparkline` over the reported
`latencyWindowDays` (7, same length as the usage window but reported
separately).

Two rules make the number trustworthy:

- **Failed attempts are counted, not averaged.** A 401 answered in 5 ms and a
  120 s timeout measure the failure, not the provider; mixing them would make
  the average reflect the error mix. The failure count is shown next to the
  average (`{n} calls · {k} failed`) so the exclusion is visible.
- **Unknown is `—`.** No successful call in the window (or no readable sample
  table at all) → no average. Only the counts, which are measured, may be `0`.

The probe stays visible and separate: `lastProbeLatencyMs` renders as
`Probe {n} ms` next to "Last checked", and the historical average is the
metric-row value. RENG-53's complaint was precisely that the two were
indistinguishable.

Retention is bounded by the write path (`RETENTION_DAYS = 30`), which prunes
once per review inside the sink's first write — the table cannot grow with the
whole history, so the window query stays a bounded index range scan.

## 3. Component Breakdown

### 3.1 ProviderCard

**Container**: `CardPanel` with `min-width: 320px; max-width: 400px;` in a responsive grid.

**Grid layout**: `display: grid; grid-template-columns: repeat(auto-fill, minmax(320px, 1fr)); gap: 16px;`

**Card structure**:
```
┌────────────────────────────────────────┐
│ [Logo]  Provider Name    [StatusBadge] │  → header row
│                                        │
│ Avg latency  Usages (7d)  Success Rate │  → metrics row (3 columns)
│ 812 ms          12           91.7%     │     + "46 calls · 3 failed"
│  ╱╲__╱╲___╱╲                           │  → recorded latency series (RENG-57)
│  Call latency · last 7 days            │
│ ██████████████████████░░░░░░░░░░░░░░░░  │  → usage-share bar (window)
│ 75% of usage (last 7 days)             │
│                                        │
│                    Last used: 09-14…   │  → recorded-use timestamp
│     Probe 320 ms · Last checked: …     │  → probe round trip + probe time
│ [Test Connection]  [Edit] […]          │  → action row
└────────────────────────────────────────┘
```

**Header row:**
- Left: provider avatar initial (32px) + provider name (16px, font-weight 600).
- Right: `StatusBadge` with status text.

**Metrics row:**
- 3 equal columns, `text-align: center`.
- Label: `font-size: 11px; color: var(--text-secondary); text-transform: uppercase; letter-spacing: 0.05em;`.
- Value: `font-family: JetBrains Mono; font-size: 18px; font-weight: 500; color: var(--text-primary);`.
- Avg-latency color: < 500ms = green, 500–1500ms = amber, > 1500ms = red.
- Success-rate color: ≥ 99% = green, 95–99% = amber, < 95% = red (forced red
  while the probe reports `error`).
- Every value is `—` when the server has no number for it. The usage metrics
  are independent of the probe: an `offline` provider that served last week's
  reviews still shows them; the latency average is independent of it too (it
  is history, not a live probe).

**Recorded latency series (RENG-57):**
- 28 six-hour buckets over the window, drawn by
  `components/common/Sparkline.vue` as SVG polylines under the metrics row.
- Buckets with no call are gaps (the line breaks); the component renders
  nothing at all with fewer than two real points, so a single event is never
  dressed up as a trend.
- Caption `Call latency · last {days} days` labels the metric above it with
  the window the server reported.

**Usage-share bar:**
- Rendered only when the payload reports an usage window (`usageAvailable`).
- `ElProgress` `:percentage="usageShare * 100"` with `stroke-width: 6`,
  color `var(--brand)`.
- Label below: "{percent}% of usage (last {days} days)" (12px, secondary);
  `—` when the window holds no usage at all.

**Action row:**
- Left: `ElButton` size="small" icon `ElIconConnection` text "Test Connection".
- Right: `ElButton` size="small" text "Edit", the set-primary star, delete.

**Status mapping:**

| Status | Badge Color | Meaning |
|--------|-------------|---------|
| healthy | green | Responding normally, latency OK |
| degraded | amber | Responding but high latency or elevated errors |
| error | red | Not responding or auth failure |
| offline | gray | Not configured or disabled |

**Where the status comes from (0.10.18, RENG-36):** `healthy` / `error` are the verdict of a real `GET {apiBase}/models` probe, cached per provider for 60 s (`src/server/api/llm_health.rs`); they are never inferred from "a key is stored", which is what used to leave a card green after its key was broken. `offline` means no key is stored (nothing is probed) and its `lastChecked` is `null` — a probe that never ran has no time to show. Editing a provider's credentials or endpoint — or deleting it — drops that provider's cached verdict, so the next read re-probes it; the dashboard's `health.llmProviders` section reads the same cache. `degraded` is not produced by the backend today. The probe's round-trip time is `lastProbeLatencyMs`, rendered as `Probe {n} ms` beside "Last checked" — it is NOT the card's latency metric (that is the recorded average, §2.3).

**Usage statistics (0.10.21, RENG-56):** every usage number is a read of
`reviews.llm_summary` (§8.2 of `src/store/traits.rs`), aggregated over the
window reported alongside them:

| Field | Meaning | `null` when |
|-------|---------|-------------|
| `requestCount` | reviews that recorded this provider in the window | no store could be read |
| `usageShare` | its share of all usage in the window (0–1) | the window holds no usage (0/0) |
| `successRate` | `completed / (completed + failed)` over those reviews | none of them reached an outcome |
| `lastUsedAt` | newest recorded use | it was not used in the window |

The envelope carries `usageWindowDays` / `usageSince` / `usageAvailable` and
`usageTotal` — every usage of the window, across all provider names, which is
the denominator of each share and therefore can exceed the sum of the cards
(a recorded provider may no longer be configured). The page's "recorded
usages" KPI shows `usageTotal`, not the sum of the visible cards.

Because `llm_summary` is written on the completion path, a review that failed
before producing a report carries no snapshot: `successRate` therefore reads
"of the reviews that used it and finished, how many completed", an upper bound
on call-level success, and the page must not present it as more than that.

**Provider data interface**:
```typescript
interface LlmProvider {
  id: string;
  name: string;
  logo: string; // SVG asset path
  status: 'healthy' | 'degraded' | 'error' | 'offline';
  lastProbeLatencyMs: number;   // probe round-trip (RENG-36; renamed in RENG-57)
  requestCount: number | null;  // RENG-56, window
  usageShare: number | null;    // RENG-56, 0–1
  successRate: number | null;   // RENG-56, 0–1
  lastUsedAt: string | null;    // RENG-56, ISO 8601
  avgLatencyMs: number | null;         // RENG-57, window, successful calls only
  latencySampleCount: number | null;   // RENG-57, the average's denominator
  latencyFailureCount: number | null;  // RENG-57, excluded from the average
  latencyLastSampleAt: string | null;  // RENG-57, newest recorded call
  latencySparkline: (number | null)[] | null; // RENG-57, 28 buckets, null = gap
  lastChecked: string | null;   // probe time, null when never probed
  configured: boolean;
}
```

### 3.2 Test Connection Flow

**Per-card test (shipped):**
- Click "Test Connection" → the clicked card's button enters its loading state (`ElLoading` spinner; the page tracks the in-flight provider id, so sibling cards stay idle).
- `POST /api/v1/llm/providers/{id}/test` — the server probes with the stored key, so no secret round-trips through the browser.
- Result shown inline above the action row as the shared `TestResultLine` (RENG-54): an outcome tag — `Connected — 234ms` / `Failed — {error message}` — with the time the probe ran and a dismiss control.
- The outcome is page-session state keyed by the provider name, deliberately outside the polled provider list, so the page's 30 s tick cannot wipe the number the user just asked for.
- A toast (`ElMessage`) reports the same outcome.

**No bulk test.** There is no "Test all providers" / "Refresh All" action, no `POST /llm/test-all` endpoint, and no card skeleton state driven by a test: the page header's only action is **Add provider**, and the cards refresh from the page's 30 s auto-refresh.

### 3.3 Latency Sparkline — shipped (RENG-57)

The card draws a real per-provider latency series: `latencySparkline` is the
mean of the successful calls in each 6-hour bucket of the window (28 points,
oldest first), read from `llm_call_samples` — the per-call rows the review path
records. `components/common/Sparkline.vue` renders it as SVG polylines.

Two rules keep it honest, and both are enforced in the component rather than
left to the caller:

- **No series, no line.** `latencySparkline` is `null` when nothing was
  recorded, and the component renders nothing with fewer than two real points
  (one call is an event, not a trend). The metrics row already shows `—` for
  the unknown average in that case.
- **Gaps stay gaps.** A bucket with no calls is `null` and breaks the line
  instead of being interpolated, so a quiet hour can never be read as a
  measured value.

## 4. Interactions & State Changes

### 4.1 Health Refresh (polling, not SSE)

- The page polls `GET /api/v1/llm/providers` every 30 s (silent — no skeleton, no error banner).
- The whole list is reconciled in place; the health metrics are plain reactive values with no count-up animation and no status-change flash on these cards.
- There is no `llm.status` SSE event: the server's SSE streams cover system/queue events and logs only.

### 4.2 Provider Configuration

- Click "Configure →" → navigate to `/config` with query param `?tab=llm&provider={providerId}`.
- On Config page, auto-scroll to LLM Settings card and pre-select the provider.

### 4.3 Empty / Unconfigured State

- If a provider is not configured (`configured: false`):
  - Show "Not Configured" badge (gray).
  - Metrics show "—" (em dash).
  - "Test Connection" button disabled.
  - "Configure →" button is the primary action (type="primary").

## 5. Responsive Behavior

| Breakpoint | Grid Columns |
|------------|--------------|
| ≥1280px | 4 columns |
| 1024–1279px | 3 columns |
| 768–1023px | 2 columns |
| <768px | 1 column |

## 6. Animation Details

- Only CSS transitions shipped on these cards: `transition: border-color/box-shadow/transform 0.2s ease` on the card and `transition: color 0.2s ease` on the metric values.
- Test result line (RENG-54): appears inline above the card's action row, no auto-dismiss — it stays until the user dismisses it.
- Not implemented on these cards: a page-enter transition, a staggered card fade-in, a status-change `flash-border` and a latency count-up.

## 7. Data Structures

```typescript
// Pinia store: llm.ts
interface LlmState {
  providers: LlmProvider[];
  loading: boolean;
  testing: Record<string, boolean>; // providerId -> isTesting
  testResults: Record<string, TestResult>;
}

interface TestResult {
  success: boolean;
  latencyMs?: number;
  error?: string;
  timestamp: string;
}

// API endpoints
GET    /api/v1/llm/providers           → { items: LlmProvider[] }
POST   /api/v1/llm/providers           → add a provider
PUT    /api/v1/llm/providers/{id}      → update a provider
DELETE /api/v1/llm/providers/{id}      → remove a provider
POST   /api/v1/llm/providers/{id}/test → TestResult

// No bulk-test or llm.status SSE endpoint exists; health comes from the 30 s
// poll of GET /llm/providers. The per-provider latency history RENG-57 draws
// is server-recorded (`llm_call_samples`, written by the review path) and
// arrives inside the same poll — there is no separate history endpoint.
```
