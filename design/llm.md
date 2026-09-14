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
- **What actually ran** — the Recent Usage card lists the newest reviews'
  `reviews.llm_summary` `provider/model` pairs (via the existing
  `GET /api/v1/reviews` list endpoint, 8 rows). A row whose usages contain
  none of the chain head's provider is tagged `未使用首选` / Primary not used,
  which is how a fallback that skipped the primary becomes visible instead of
  looking like a normal run.

Per-request counts, latency and sparkline stay out of scope (RENG-56/57) — the
card metrics remain render-only placeholders.

## 3. Component Breakdown

### 3.1 ProviderCard

**Container**: `CardPanel` with `min-width: 320px; max-width: 400px;` in a responsive grid.

**Grid layout**: `display: grid; grid-template-columns: repeat(auto-fill, minmax(320px, 1fr)); gap: 16px;`

**Card structure**:
```
┌────────────────────────────────────────┐
│ [Logo]  Provider Name    [StatusBadge] │  → header row
│                                        │
│ Latency        Requests      Errors      │  → metrics row (3 columns)
│ 234 ms         1,204         0.2%       │
│                                        │
│ ██████████████████████████████░░░░░░░░   │  → usage bar (optional)
│ 74% capacity                           │
│                                        │
│ [Test Connection]  [Configure →]         │  → action row
└────────────────────────────────────────┘
```

**Header row:**
- Left: Provider logo (custom SVG, 32px) + provider name (16px, font-weight 600).
- Right: `StatusBadge` with status text.

**Metrics row:**
- 3 equal columns, `text-align: center`.
- Label: `font-size: 11px; color: var(--text-secondary); text-transform: uppercase; letter-spacing: 0.05em;`.
- Value: `font-family: JetBrains Mono; font-size: 18px; font-weight: 500; color: var(--text-primary);`.
- Latency color: < 500ms = green, 500–1500ms = amber, > 1500ms = red.
- Error rate color: < 1% = green, 1–5% = amber, > 5% = red.

**Usage bar (optional, if available):**
- `ElProgress` `:percentage="usagePercent"` with `stroke-width: 6`.
- Color: `var(--brand)`.
- Label below: "{usagePercent}% capacity" (12px, secondary).

**Action row:**
- Left: `ElButton` size="small" icon `ElIconConnection` text "Test Connection".
- Right: `ElButton` size="small" text "Configure →" (links to `/config` with provider pre-selected, or opens inline config drawer).

**Status mapping:**

| Status | Badge Color | Meaning |
|--------|-------------|---------|
| healthy | green | Responding normally, latency OK |
| degraded | amber | Responding but high latency or elevated errors |
| error | red | Not responding or auth failure |
| offline | gray | Not configured or disabled |

**Where the status comes from (0.10.18, RENG-36):** `healthy` / `error` are the verdict of a real `GET {apiBase}/models` probe, cached per provider for 60 s (`src/server/api/llm_health.rs`); they are never inferred from "a key is stored", which is what used to leave a card green after its key was broken. `offline` means no key is stored (nothing is probed). Editing a provider's credentials or endpoint — or deleting it — drops that provider's cached verdict, so the next read re-probes it; the dashboard's `health.llmProviders` section reads the same cache. `degraded` is not produced by the backend today (no latency/error-rate statistics yet), and `latencyMs` is the probe's own round-trip time.

**Provider data interface**:
```typescript
interface LlmProvider {
  id: string;
  name: string;
  logo: string; // SVG asset path
  status: 'healthy' | 'degraded' | 'error' | 'offline';
  latencyMs: number;
  requestCount: number;
  errorRate: number; // 0.0 – 1.0
  usagePercent?: number;
  lastChecked: string; // ISO 8601
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

### 3.3 Historical Sparkline (Optional Enhancement)

**Inside each card, below metrics:**
- A mini line chart (SVG or CSS) showing latency over last 24h.
- `height: 40px; width: 100%;`
- Line color: `var(--brand)` with `opacity: 0.6`.
- No axes, no labels — pure visual trend.
- Data: `number[]` of 24 hourly latency averages.

**Implementation**: Pure SVG `<polyline>` or `<path>` inside the card. No external chart library.

```svg
<svg viewBox="0 0 100 40" preserveAspectRatio="none" style="width: 100%; height: 40px;">
  <polyline points="0,30 10,25 20,28 ..." fill="none" stroke="var(--brand)" stroke-width="2" opacity="0.6"/>
</svg>
```

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
- Not implemented on these cards: a page-enter transition, a staggered card fade-in, a status-change `flash-border`, a latency count-up, and the sparkline below.

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

// No bulk-test, per-provider latency-history or llm.status SSE endpoint
// exists; health comes from the 30 s poll of GET /llm/providers.
```
