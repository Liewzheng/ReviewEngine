# Configuration Schema Reference

Configuration is done via `.code-audit-config.toml` in the project root or `~/.config/review-engine/.code-audit-config.toml` for user-level config.

## File Format

The config file uses TOML format. Below is the complete schema with all available sections.

## Top-level Fields

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `output_dir` | string | `~/.config/review-engine/reports/` | Directory for auto-saved reports |
| `max_team_size` | integer (optional) | `6` | Maximum number of experts per review |
| `max_concurrent_llm_calls` | integer (optional) | `6` | Maximum concurrent LLM API calls |

## `[project]`

| Field | Type | Description |
|-------|------|-------------|
| `name` | string (optional) | Project name for display |
| `project_type` | string (optional) | Project type: `embedded`, `web`, `mobile`, `backend`, `desktop` |
| `os` | string (optional) | Target operating system, e.g. `Linux`, `RTOS`, `bare-metal` |
| `arch` | string (optional) | Target CPU architecture, e.g. `ARM`, `x86_64`, `RISC-V` |
| `domain` | string (optional) | Application domain, e.g. `IoT`, `fintech`, `consumer` |
| `constraints` | string (optional) | Extra project constraints that affect review relevance |

## `[report]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `aggregated` | boolean | `false` | Whether to produce an aggregated report |
| `max_findings_per_expert` | integer | `5` | Max findings per expert in the prompt |

## `[scoring]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | boolean | `true` | Enable/disable scoring |
| `display_individual_scores` | boolean | `true` | Show individual expert scores |
| `display_weighted_score` | boolean | `true` | Show weighted overall score |
| `consensus_threshold` | integer | `70` | Consensus threshold for high-confidence findings (1-100) |

### `[scoring.penalties]`

Penalty points deducted per finding severity. All default to built-in values.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `critical` | integer | `30` | Points deducted for each Critical finding |
| `high` | integer | `15` | Points deducted for each High finding |
| `medium` | integer | `5` | Points deducted for each Medium finding |
| `low` | integer | `1` | Points deducted for each Low finding |
| `note` | integer | `0` | Points deducted for each Note finding |

### `[scoring.risk_thresholds]`

Score-to-risk-level mapping thresholds. Scores are compared with `<=`.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `critical_max` | integer | `40` | Scores ≤ this are Critical |
| `high_max` | integer | `60` | Scores ≤ this (but > critical_max) are High |
| `medium_max` | integer | `80` | Scores ≤ this (but > high_max) are Medium |
| `low_max` | integer | `95` | Scores ≤ this (but > medium_max) are LowMedium |

```toml
[scoring]
enabled = true
display_individual_scores = true
display_weighted_score = true
consensus_threshold = 70

[scoring.penalties]
critical = 30
high = 15
medium = 5
low = 1
note = 0

[scoring.risk_thresholds]
critical_max = 40
high_max = 60
medium_max = 80
low_max = 95
```

## `[commands]`

Command enable/disable flags. All commands are disabled by default.

```toml
[commands]
review = true
describe = false
improve = false
ask = false
repo_review = false
update_changelog = false
```

## `[[llm]]`

LLM provider configuration. Multiple providers can be configured for fallback.

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `provider` | string | yes | Provider name: `openai`, `anthropic`, or any custom name |
| `model` | string | yes | Model name (e.g., `gpt-4o`, `claude-sonnet-4-20250514`) |
| `api_key` | string | no* | API key (use env var for production) |
| `api_base` | string | no | API base URL (defaults to provider standard) |
| `max_tokens` | integer | no | Max tokens per response (default: `4096`) |
| `temperature` | float | no | Temperature for generation (default: `0.3`) |

Priority: `--llm-config` CLI > `LLM_CONFIG` env var > `[[llm]]` TOML.

```toml
[[llm]]
provider = "openai"
model = "gpt-4o"
api_key = "sk-..."
api_base = "https://api.openai.com/v1"
max_tokens = 4096
temperature = 0.3
```

## `[review_experts.<name>]`

Expert role configuration. Each key under `[review_experts]` defines one expert.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | boolean | `true` | Whether this expert participates |
| `weight` | integer | `0` | Score weight (all enabled experts must sum to 100) |
| `model` | string | `""` | Per-expert model override (empty = use default) |
| `title` | string | `""` | Professional title |
| `role` | string | `""` | Role description (required when enabled) |
| `style` | string | `""` | Review style description |
| `commands` | string[] | `[]` | Commands this expert participates in |
| `principles` | string[] | `[]` | Judgment principles |
| `focus` | string[] | `[]` | Focus areas |
| `standards` | string[] | `[]` | Reference standards |
| `prompt` | string | `""` | System prompt for the expert |
| `trigger` | string/table | none | Trigger condition: `"always"`, `"on_demand"`, `{patterns=[...]}`, `{languages=[...]}`, or `{max_files=N}` |

```toml
[review_experts.lead]
enabled = true
weight = 20
commands = ["review", "describe"]
title = "Staff Engineer"
role = "Lead Reviewer"
style = "concise, synthesizes team input"
prompt = "You are the Lead Reviewer..."
```

## `[diff]`

Large PR detection and chunking configuration. Controls when compression, chunking, and parallel review are triggered.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `max_input_tokens` | integer | `120000` | LLM context window limit (tokens); exceeding this triggers chunking |
| `max_tokens_per_chunk` | integer | `30000` | Token budget per chunk |
| `large_pr_file_threshold` | integer | `21` | PRs with more files than this are treated as large PRs |
| `large_pr_line_threshold` | integer | `1000` | PRs with more changed lines than this are treated as large PRs |
| `compression_level` | string | `"aggressive"` | Compression level: `"none"` / `"light"` / `"medium"` / `"aggressive"` |
| `chunking_strategy` | string | `"adaptive"` | Chunking strategy: `"files"` / `"hunks"` / `"adaptive"` |
| `max_chunks_per_expert` | integer | `3` | Maximum number of chunks each expert receives |

**Detection logic:**

Large PR detection happens in two phases:

1. **Pre-parse estimate**: A byte threshold is estimated from `large_pr_line_threshold × 50` (default 1000 × 50 = 50000 bytes), used to choose the appropriate progress stage (`small_pr` / `large_pr`).
2. **Exact assessment (post-parse)**: `assess_large_pr()` checks three dimensions (file count > `large_pr_file_threshold`, changed lines > `large_pr_line_threshold`, estimated tokens > `max_input_tokens`). If any exceed the threshold, the compression/chunking pipeline is triggered.

```toml
[diff]
max_input_tokens = 120000
max_tokens_per_chunk = 30000
large_pr_file_threshold = 21
large_pr_line_threshold = 1000
compression_level = "aggressive"
chunking_strategy = "adaptive"
max_chunks_per_expert = 3
```

## `[rate_limit]`

LLM API rate-limit configuration. Controls concurrent request count and token consumption rate to avoid hitting 429 limits.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `max_rpm` | integer | `60` | Maximum requests per minute |
| `max_tpm` | integer | `200000` | Maximum tokens per minute (input + output) |
| `window_seconds` | integer | `60` | Sliding window size in seconds |

```toml
[rate_limit]
max_rpm = 60
max_tpm = 200000
window_seconds = 60
```

## Configuration Loading Order

1. Built-in defaults (`docs/code-audit-default.toml`) with environment overrides
2. User-level config (`~/.config/review-engine/.code-audit-config.toml`)
3. Project-level config (`.code-audit-config.toml` in the project root)
4. Environment variables (`LLM_CONFIG`, `CODE_AUDIT_COMMANDS`, etc.)
5. CLI arguments (`--llm-config`, `--config`)
