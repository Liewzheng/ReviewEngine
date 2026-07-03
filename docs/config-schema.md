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

大 PR 检测和分块配置。控制何时触发压缩、分块、并行审核。

| 字段 | 类型 | 默认值 | 描述 |
|------|------|--------|------|
| `max_input_tokens` | integer | `120000` | LLM 上下文窗口上限（token），超过后触发分块 |
| `max_tokens_per_chunk` | integer | `30000` | 每个 chunk 的 token 预算上限 |
| `large_pr_file_threshold` | integer | `21` | 文件数超过此值视为大 PR |
| `large_pr_line_threshold` | integer | `1000` | 变更行数超过此值视为大 PR |
| `compression_level` | string | `"aggressive"` | 压缩级别：`"none"` / `"light"` / `"medium"` / `"aggressive"` |
| `chunking_strategy` | string | `"adaptive"` | 分块策略：`"files"` / `"hunks"` / `"adaptive"` |
| `max_chunks_per_expert` | integer | `3` | 每位专家最多接收多少个 chunk |

**判定机制：**

大 PR 的判定分两个阶段：

1. **预判（parse diff 前）**：从 `large_pr_line_threshold × 50` 估算字节阈值（默认 1000 × 50 = 50000 bytes），用于选择合适的进度阶段（`small_pr` / `large_pr`）。
2. **精确判定（parse diff 后）**：`assess_large_pr()` 检查三个维度（文件数 > `large_pr_file_threshold`、变更行数 > `large_pr_line_threshold`、预估 token > `max_input_tokens`），任一超标即触发压缩/分块流程。

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

LLM API 速率限制配置。控制并发请求数和 token 消耗速率，防止触发 429 限流。

| 字段 | 类型 | 默认值 | 描述 |
|------|------|--------|------|
| `max_rpm` | integer | `60` | 每分钟最大请求数 |
| `max_tpm` | integer | `200000` | 每分钟最大 token 消耗（输入+输出）|
| `window_seconds` | integer | `60` | 滑动窗口大小（秒）|

```toml
[rate_limit]
max_rpm = 60
max_tpm = 200000
window_seconds = 60
```

## Configuration Loading Order

1. Built-in defaults (`docs/code-audit-default.toml`)
2. Project config (`.code-audit-config.toml` in project root)
3. Legacy config (`.pr-agent.toml` — deprecated)
4. User-level config (`~/.config/review-engine/.code-audit-config.toml`)
5. Environment variables (`LLM_CONFIG`, `CODE_AUDIT_COMMANDS`, etc.)
6. CLI arguments (`--llm-config`, `--config`)
