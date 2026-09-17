# Experts Management Page Design

## 1. Route & Purpose

- **Route**: `/experts`
- **Purpose**: Manage the LLM expert pool — enable/disable individual experts, adjust weights, and inspect their behavior. Each expert is a specialized reviewer (security, performance, etc.) backed by an LLM prompt and configuration.
- **Data**: Fetched on mount. SSE updates reflect expert status changes triggered by other parts of the system.

## 2. Page Layout

```
PageHeader
├── Title: "Experts Management" + subtitle: "Configure LLM review experts"
└── Right: Add Expert button (ElButton icon: Plus, type: primary) — optional future feature

Experts Grid
├── ExpertCard
├── ExpertCard
├── ExpertCard
└── ...
```

## 3. Component Breakdown

### 3.1 ExpertCard

**Container**: `CardPanel` with `min-width: 280px; max-width: 360px;` in a responsive grid.

**Grid layout**: `display: grid; grid-template-columns: repeat(auto-fill, minmax(280px, 1fr)); gap: 16px;`

**Card structure**:
```
┌────────────────────────────────────────┐
│ [Icon]  Expert Name          [Toggle]  │  → header row
│ Category: Security                     │  → category tag
│                                        │
│ Weight: ████████░░░░  80%              │  → weight slider (read-only or editable)
│                                        │
│ Reviews security vulnerabilities,      │  → description (2 lines max, ellipsis)
│ injection risks, and secret leaks.     │
│                                        │
│ [View Details]  [Edit]                 │  → actions
└────────────────────────────────────────┘
```

**Header row:**
- Left: Expert icon (custom SVG, 36px, colored by category) + expert name (16px, font-weight 600).
- Right: `ElSwitch` for enable/disable. Label hidden (icon-only switch with tooltip).
  - Switch ON: `active-color: var(--success)`.
  - Switch OFF: `inactive-color: var(--offline)`.
  - Tooltip on hover: "Enabled" / "Disabled".

**Category tag:**
- `ElTag` size="small" below the name.
- Categories: Security, Performance, Quality, Maintainability, Test Coverage, Documentation, Dependencies, Accessibility, Architecture.
- Each category has a fixed color mapping:
  - Security → red (`#ef4444`)
  - Performance → amber (`#f59e0b`)
  - Quality → green (`#22c55e`)
  - Maintainability → blue (`#3b82f6`)
  - Test Coverage → purple (`#a855f7`)
  - Documentation → gray (`#6b7280`)
  - Dependencies → indigo (`#6366f1`)
  - Accessibility → pink (`#ec4899`)
  - Architecture → teal (`#14b8a6`)

**Weight slider:**
- `ElSlider` with `disabled: !isEditing` (if editing enabled globally).
- `show-stops: true`, `step: 5`, `max: 100`.
- Value label to the right: "{value}%".
- In read-only mode: slider is disabled but visible; value shown as text.

**Description:**
- `font-size: 13px; color: var(--text-secondary); line-height: 1.5;`
- Max 3 lines. If longer, truncate with ellipsis and show full text in tooltip.

**Action buttons:**
- "View Details": `ElButton` size="small" type="default" icon `ElIconView`. Opens detail modal.
- "Edit": `ElButton` size="small" type="primary" icon `ElIconEdit`. Enables inline editing for this card (weight slider becomes active, description editable).

### 3.2 Expert Detail Modal

**Component**: `ElDialog` with `width: 600px` (full width on mobile).

**Dialog content**:
```
Expert Name
[Category Tag]

Enabled: [Toggle]
Weight: [Slider] 80%

Description:
[ElInput type="textarea" readonly or editable]

Prompt Preview:
[ElInput type="textarea" readonly, rows: 8, font-family: JetBrains Mono]

Last 5 Reviews:
├── DataTable (small, 3 columns: MR, Score, Date)
```

**Prompt preview**: Read-only textarea with the LLM system prompt for this expert. Rendered in `JetBrains Mono` with syntax highlighting disabled (plain text).

**Last reviews table**: Shows recent review results from this expert. Columns: MR Title, Score (0–100), Date. Clicking a row navigates to `/history?reviewId={id}`.

**Dialog actions**:
- "Close" (default): closes modal.
- "Save Changes" (primary, visible only in edit mode): saves weight/description changes.

### 3.3 Enable / Disable Interaction

- Toggle `ElSwitch` on a card → immediate visual feedback (card border color changes: enabled = default, disabled = `opacity: 0.6; border-color: var(--offline);`).
- `PUT /api/v1/system/experts/{id}` with body `{ enabled: boolean }`.
- On success: `ElNotification` "{ExpertName} {enabled/disabled}" (success, 2000ms).
- On error: revert switch state, `ElNotification` error.
- Optimistic UI: update switch state immediately, revert on error.
- **Persistence (RENG-69)**: the server applies the change to the running config AND stores it as an override (per-expert `enabled`/`weight`) in the configuration database — DB over the file's `[review_experts]`, so a restart keeps the edit while every expert the UI never touched keeps its file value. The response carries `persisted`:
  - `true` → the edit survives a restart;
  - `false` → no database is attached (`REVIEW_DISABLE_DB=1`, embedded use): the edit is memory-only and the page shows a warning notification (`experts.memoryOnlyTitle` / `experts.memoryOnlyMessage`) instead of the success one;
  - a failed write is a `500` → the error path above fires; a change is never reported as saved when it was not.

### 3.4 Weight Editing

- Enter edit mode (global "Edit" button or per-card edit) → weight slider becomes active.
- Drag slider → value updates in real time with debounced `PUT /api/v1/system/experts/{id}` (debounce: 500ms after drag ends).
- On save: subtle green flash on the card; a memory-only (`persisted: false`) commit shows the same warning as the toggle instead.
- Weight affects how much the expert's opinion contributes to the final review score.
- The `sum to 100` rule is validated for config files (`review-engine validate`); the slider stores the value as-is within the schema's 0–100 range (the API refuses anything higher with `422`).

## 4. Interactions & State Changes

### 4.1 Card State Transitions

**Disabled card:**
- `opacity: 0.6;`
- Icon: grayscale filter (`filter: grayscale(100%);`).
- "Disabled" badge appears next to category tag.
- Progress bar / weight slider: dimmed.

**Enabled card:**
- Full opacity.
- Full color icon.
- No disabled badge.

**Transition**: `transition: opacity 0.2s ease, filter 0.2s ease;`.

### 4.2 SSE Updates

- Listen to `system.health` for expert status changes triggered externally.
- On update: match expert by ID, update card state, add `flash-border` animation if weight or status changed.

### 4.3 Global Edit Mode

- Optional: global "Edit Mode" toggle in PageHeader.
- When ON: all weight sliders become editable, all "Edit" buttons change to "Save".
- When OFF: all changes are persisted, sliders disabled.
- Unsaved changes guard: same as Config page (`beforeunload` + router guard).

## 5. Responsive Behavior

| Breakpoint | Grid Columns |
|------------|--------------|
| ≥1280px | 4 columns |
| 1024–1279px | 3 columns |
| 768–1023px | 2 columns |
| <768px | 1 column |

## 6. Animation Details

- Page enter: `page-enter` transition.
- Card enter: staggered fade-in, delay = index * 60ms, `0.3s ease`.
- Card hover: standard `CardPanel` hover effect.
- Toggle switch: `ElSwitch` built-in animation.
- Enable/disable transition: `opacity` + `grayscale` transition, `0.2s ease`.
- Weight slider change: `transition: all 0.1s ease` on slider handle.
- Detail modal: `ElDialog` default scale + fade animation (0.3s).
- Save success: `flash-border` on card (0.6s).

## 7. Data Structures

```typescript
// Pinia store: experts.ts
interface ExpertsState {
  experts: Expert[];
  loading: boolean;
  isEditing: boolean;
  dirty: boolean;
}

interface Expert {
  id: string;
  name: string;
  category: ExpertCategory;
  icon: string; // SVG asset path
  enabled: boolean;
  weight: number; // 0–100
  description: string;
  prompt: string; // full effective prompt (RENG-93): override > config file > default
  promptOverride: boolean; // true when `prompt` was authored in the WebUI
  lastReviews: ExpertReviewSummary[];
}

type ExpertCategory =
  | 'security'
  | 'performance'
  | 'quality'
  | 'maintainability'
  | 'test-coverage'
  | 'documentation'
  | 'dependencies'
  | 'accessibility'
  | 'architecture';

interface ExpertReviewSummary {
  reviewId: string;
  mrTitle: string;
  score?: number;
  date: string;
}

// API endpoints (0.10.x: the toggle/weight POST endpoints were folded into
// one PUT per expert; the response carries `persisted`, see §3.3; RENG-95 adds
// the report-level aggregation flag)
GET /api/v1/system/experts                 → { experts: Expert[], aggregated: boolean }
PUT /api/v1/system/experts/{id}            → Expert & { persisted: boolean }  // enabled and/or weight and/or prompt (RENG-93)
PUT /api/v1/system/experts/aggregated      → { aggregated: boolean, persisted: boolean }  // RENG-95
```

### 3.6 Aggregation toggle (RENG-95)

- A page-level switch「聚合报告 / Aggregated report」controls the report-level `report.aggregated` flag. The aggregator expert runs only when this flag **and** the `aggregator` expert's enabled state both hold (`select_aggregator_expert`); the switch's hint states the rule.
- The aggregator's card shows the two-condition state on the row itself: a warning tag「已启用但聚合未开启」when the expert is enabled but the flag is off, a success tag when both are on. The detail drawer of the aggregator carries the same switch, tying the flag to the expert it gates.
- `PUT /api/v1/system/experts/aggregated` with body `{ "aggregated": bool }`: hot-applies to the running config, persists to the same `ui` row `PUT /config` writes (a config-page save that omits it keeps the stored value — tri-state `Option<bool>`), and publishes the dispatch-time override review paths re-apply over the config they resolve. Without a DB the response is `persisted: false` (memory-only, lost on restart) — the same honesty rule as §3.3.
- Optimistic UI, identical to the per-expert switch: flip locally, PUT, adopt the server echo, revert + error notification on failure; the in-flight guard pauses the background poll so a tick cannot snap the switch back.
