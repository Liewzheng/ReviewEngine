# LLM Status Page Design

## 1. Route & Purpose

- **Route**: `/llm`
- **Purpose**: Monitor the health and performance of each LLM provider. Quick diagnostic view for troubleshooting review failures or latency spikes.
- **Data**: Fetched on mount + refresh button. SSE `llm.status` updates individual provider cards in real time.

## 2. Page Layout

```
PageHeader
├── Title: "LLM Status" + subtitle: "Provider health and performance"
└── Right: Refresh All button (ElButton icon: Refresh)

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

**Per-card test:**
- Click "Test Connection" → button enters loading state (`ElLoading` spinner).
- `POST /api/v1/llm/{providerId}/test`
- Result shown inline below the action row:
  - Success: `ElAlert` type="success" title="Connected" description="Latency: 234ms" closable.
  - Failure: `ElAlert` type="error" title="Connection failed" description="{error message}" closable.
- Alert auto-dismisses after 5 seconds unless hovered.

**Bulk test (Refresh All):**
- Click "Refresh All" in PageHeader → all cards show skeleton state simultaneously.
- `POST /api/v1/llm/test-all`
- Results update all cards at once.
- `ElNotification` summary: "All providers tested — {N} healthy, {M} issues".

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

### 4.1 SSE Updates

- Listen to `llm.status` events.
- Payload: `{ providerId: string; status: LlmProviderStatus; latencyMs?: number; }`
- Update the matching `ProviderCard` in place.
- If status changes (e.g., healthy → degraded), trigger `flash-border` animation on the card.
- Update latency value with smooth number transition (count-up animation, 0.3s).

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

- Page enter: `page-enter` transition.
- Card enter: staggered fade-in, delay = index * 50ms, `0.25s ease`.
- Card status change: `flash-border` animation (0.6s).
- Latency number update: `transition: color 0.2s ease` (flash green/amber/red).
- Test result alert: slide down `0.2s ease`, auto-dismiss fade-out `0.3s ease`.
- Sparkline: SVG path draws on mount using `stroke-dasharray` / `stroke-dashoffset` animation (1s ease).

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
GET  /api/v1/llm/providers       → LlmProvider[]
POST /api/v1/llm/{id}/test       → TestResult
POST /api/v1/llm/test-all        → Record<string, TestResult>
GET  /api/v1/llm/{id}/latency    → number[] (24h hourly averages)

// SSE event: llm.status
interface LlmStatusEvent {
  providerId: string;
  status: 'healthy' | 'degraded' | 'error' | 'offline';
  latencyMs?: number;
  errorRate?: number;
  timestamp: string;
}
```
