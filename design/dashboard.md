# Dashboard Page Design

## 1. Route & Purpose

- **Route**: `/` or `/dashboard`
- **Purpose**: At-a-glance system overview. The first page users see after loading the app.
- **Data freshness**: KPIs and health status refreshed on mount; 24h trend and recent activity table updated via SSE `system.health` and `queue.update` events.

## 2. Page Layout

```
PageHeader
├── Title: "Dashboard" + subtitle: "System overview and recent activity"
└── Right: Refresh button (ElButton icon: Refresh) + timestamp of last update

Row 1 — KPI Cards (4 columns, gap: 16px)
├── KpiCard: Reviews This Week
├── KpiCard: Active Queue
├── KpiCard: Success Rate
└── KpiCard: Avg Duration

Row 2 — Two columns (gap: 16px, margin-top: 24px)
├── Left (70%): 24h Activity Trend card
└── Right (30%): System Health card

Row 3 — Recent Activity Table (margin-top: 24px)
└── CardPanel: Recent Reviews (Top 10)
```

## 3. Component Breakdown

### 3.1 KPI Cards Row

**Container**: `display: grid; grid-template-columns: repeat(4, 1fr); gap: 16px;`
**Responsive**:
- `< 1024px`: `grid-template-columns: repeat(2, 1fr);`
- `< 640px`: `grid-template-columns: 1fr;`

Each card is a **KpiCard** (shared component) with these specifics:

| Card | Icon | Value | Trend | Format |
|------|------|-------|-------|--------|
| Reviews This Week | `ElIconDocument` | integer count | vs last week | `1,234` |
| Active Queue | `ElIconRefresh` | integer count | — | `12` |
| Success Rate | `ElIconCheck` | percentage | vs yesterday | `98.2%` |
| Avg Duration | `ElIconTimer` | duration string | vs last week | `4m 32s` |

**Trend logic:**
- Upward trend (good): green arrow-up + percentage. Example: `↑ 5% vs last week`
- Downward trend (bad): red arrow-down + percentage. Example: `↓ 2% vs yesterday`
- Neutral: gray dash. Example: `— no change`
- Trend text color: `var(--success)` for good, `var(--error)` for bad, `var(--text-secondary)` for neutral.
- Trend arrow uses `ElIconArrowUp` / `ElIconArrowDown` / `ElIconMinus`.

**Loading state**: Show 4 `ElSkeleton` cards with `animated: true`.

### 3.2 24h Activity Trend Chart

**Container**: `CardPanel` with title "24h Activity Trend" + `ElIconTrendCharts` icon.

**Chart library**: `lightweight-charts` (single line series).
**Dimensions**: `height: 280px; width: 100%;`

**Chart configuration:**
```typescript
const chartOptions = {
  layout: {
    background: { color: 'transparent' },
    textColor: 'var(--text-secondary)',
  },
  grid: {
    vertLines: { color: 'var(--border-color)', style: LineStyle.SparseDotted },
    horzLines: { color: 'var(--border-color)', style: LineStyle.SparseDotted },
  },
  crosshair: { mode: CrosshairMode.Magnet },
  rightPriceScale: { borderColor: 'var(--border-color)' },
  timeScale: { borderColor: 'var(--border-color)', timeVisible: true },
  handleScroll: false,
  handleScale: false,
};

const lineSeries = chart.addLineSeries({
  color: 'var(--brand)',
  lineWidth: 2,
  crosshairMarkerVisible: true,
  crosshairMarkerRadius: 4,
  crosshairMarkerBorderColor: 'var(--brand)',
  crosshairMarkerBackgroundColor: 'var(--bg-primary)',
});
```

**Data format**:
```typescript
interface TrendPoint {
  time: number;       // Unix timestamp in seconds
  value: number;      // Review count in that hour
}
```

**Tooltip**: Custom lightweight-charts tooltip showing time + count. Styled with `background: var(--bg-card); border: 1px solid var(--border-color); border-radius: var(--radius-sm); padding: 8px 12px; font-family: JetBrains Mono; font-size: 12px; color: var(--text-primary);`.

**Empty state**: If no data in last 24h, show centered text "No activity in the last 24 hours" in `--text-secondary` with `ElIconInfoFilled` icon above.

**Loading state**: `ElSkeleton` with `rows: 5` and `animated: true` inside the card body.

### 3.3 System Health Card

**Container**: `CardPanel` with title "System Health" + `ElIconFirstAidKit` icon + refresh button.

**Layout**:
```
System Health
├── Integration Status   (RENG-97: probe-cache verdicts, never entry presence)
│   ├── GitLab API      [green dot]  Configured · Last test: 3 min ago
│   ├── GitHub API      [gray dot]   Not configured
│   └── GitLab API      [blue dot]   Not probed yet
│
├── LLM Providers
│   ├── OpenAI GPT-4    [green dot]  Healthy  ·  234ms
│   ├── Anthropic Claude [yellow dot] Degraded  ·  1.2s
│   └── Local Ollama    [red dot]    Error  ·  Connection refused
│
└── Overall Status: [green badge] All Systems Operational
```

**Health item row style:**
- `display: flex; justify-content: space-between; align-items: center; padding: 10px 0; border-bottom: 1px solid var(--border-color);`
- Last item: `border-bottom: none;`
- Left: icon (16px) + service name (13px, `--text-primary`) + optional config badge (12px, `--text-secondary`)
- Right: `StatusBadge` (dot-only or text) + latency (12px, JetBrains Mono, `--text-secondary`)

**Integration status source (RENG-97)**: the integration rows report the
platform probe cache — the Configuration page's `POST /config/git-platforms/test`
is the single source of truth that the dashboard and `GET /system/health` both
read. Entry presence (a `git_platforms` row, or a startup env/CLI token flag)
is never health: a configured-but-unprobed integration reads `unknown`
("Not probed yet"), a failed probe reads `error` with the failure message, and
only a real successful probe reads `success` (carrying the probe's `latencyMs`
and `checkedAt`, so a stale result is visible).

**Overall status**: Full `StatusBadge` at bottom of card, centered or right-aligned.

**Data structure**:
```typescript
interface HealthStatus {
  service: string;
  type: 'integration' | 'llm';
  status: 'success' | 'warning' | 'error' | 'offline' | 'unknown';
  latencyMs?: number;  // real probe round-trip; present only after a successful probe
  checkedAt?: string;  // probe timestamp (RFC3339); absent when nothing was probed
  message?: string;
}

interface SystemHealth {
  integrations: HealthStatus[];
  llmProviders: HealthStatus[];
  overall: 'success' | 'warning' | 'error';
  lastChecked: string; // ISO 8601
}
```

**Refresh button**: `ElButton` size="small" icon `ElIconRefresh`. On click: re-fetch health data, show `ElLoading` spinner on the card.

### 3.4 Recent Activity Table

**Container**: `CardPanel` with title "Recent Reviews" + `ElIconDocument` + link to `/history` ("View All →").

**Table**: `DataTable` (shared component) with 5 columns.

**Columns:**
| Column | Width | Content |
|--------|-------|---------|
| MR Title | flexible | Title text + `ElTag` size="small" for project prefix |
| Author | 140px | Avatar (24px circle) + name |
| Status | 100px | `StatusBadge` |
| Duration | 100px | JetBrains Mono text (e.g., `3m 12s`) |
| Time | 160px | Relative time (e.g., "2 min ago") + absolute tooltip on hover |

**Row data interface**:
```typescript
interface RecentReview {
  id: string;
  mrTitle: string;
  project: string;
  author: { name: string; avatarUrl?: string };
  status: 'success' | 'failed' | 'running' | 'queued';
  durationMs: number;
  createdAt: string; // ISO 8601
}
```

**Status mapping:**
- `success` → `StatusBadge` with `status="success"`, text "Completed"
- `failed` → `StatusBadge` with `status="error"`, text "Failed"
- `running` → `StatusBadge` with `status="running"`, text "In Progress"
- `queued` → `StatusBadge` with `status="queued"`, text "Queued"

**Row hover**: highlight + pointer cursor. Clicking a row navigates to `/history?reviewId={id}` (or opens detail drawer if implemented).

**Loading state**: 5 skeleton rows.

**Empty state**: Centered message "No recent reviews" with `ElIconInfoFilled` icon.

## 4. Interactions & State Changes

### 4.1 Refresh Button

- On click: set `isRefreshing = true` (adds `ElIconLoading` spin animation to button).
- Re-fetch all dashboard data in parallel (KPIs, trend, health, recent activity).
- On success: update Pinia store, show `ElNotification` "Dashboard refreshed" (type: success, duration: 2000ms).
- On error: show `ElNotification` with error message (type: error, duration: 5000ms).
- `isRefreshing = false` when all requests complete.

### 4.2 SSE Updates

Listen to `queue.update` and `system.health` events.

**Queue update**: If the update affects a review in the recent activity table, update that row in place and add the `sse-update` animation class (flash border).

**Health update**: Update the System Health card values in place. If overall status changes (e.g., success → warning), add `sse-update` animation to the card.

### 4.3 Auto-Refresh

- KPIs and health: auto-refresh every 60 seconds via `setInterval` (cleared on component unmount).
- Trend chart: static after initial load; refreshed only on manual refresh or page revisit.

## 5. Responsive Behavior

| Breakpoint | Layout Changes |
|------------|----------------|
| ≥1280px | 4 KPI cards, 70/30 trend+health split |
| 1024–1279px | 4 KPI cards, 60/40 trend+health split |
| 768–1023px | 2 KPI cards per row, trend+health stack vertically |
| <768px | 1 KPI card per row, all sections stack, table horizontal scroll |

## 6. Animation Details

- Page enter: `page-enter` transition (fade + slideY, 0.2s).
- KPI cards: staggered fade-in on load. Card 1: delay 0ms, Card 2: delay 50ms, Card 3: delay 100ms, Card 4: delay 150ms. Each: `opacity: 0 → 1`, `translateY: 8px → 0`, `duration: 0.3s`, `ease: cubic-bezier(0.4, 0, 0.2, 1)`.
- Chart: line draws from left to right on first load using lightweight-charts built-in animation.
- Table rows: no entrance animation; SSE updates trigger `flash-border` (0.6s).

## 7. Data Fetching

```typescript
// Pinia store: dashboard.ts
interface DashboardState {
  kpis: KpiData | null;
  trend: TrendPoint[];
  health: SystemHealth | null;
  recentReviews: RecentReview[];
  loading: boolean;
  lastUpdated: string | null;
}

// API endpoints
GET /api/v1/dashboard/kpis      → KpiData
GET /api/v1/dashboard/trend      → TrendPoint[]
GET /api/v1/dashboard/health     → SystemHealth
GET /api/v1/dashboard/recent     → RecentReview[] (limit 10)
```
