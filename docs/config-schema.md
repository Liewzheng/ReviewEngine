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
| `min_confidence` | integer | `6` | Minimum confidence (0-10) for a finding; findings below this have their severity downgraded one level by the lead consolidator |
| `drop_low_confidence` | boolean | `false` | When `true`, findings below `min_confidence` are dropped entirely instead of downgraded |
| `verification_pass` | boolean | `false` | Extra LLM pass that re-checks each finding against the diff hunks, the referenced file's full content, and the changed-file list; drops findings the evidence disproves (fail-open, adds LLM cost) |
| `verification_max_file_bytes` | integer | `20000` | Max bytes of referenced file content injected into the verification prompt |
| `feedback_filtering` | boolean | `true` | When `true`, findings previously marked as false positives via the feedback API (matched by stable fingerprint) are filtered out of subsequent reviews and listed in `dropped_findings`; fail-open when the feedback file is missing or unreadable |

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

Score-to-risk-level mapping thresholds. The `*_max` fields are compared with `<=`; `healthy_min` is compared with `>` and takes precedence over the other bands.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `critical_max` | integer | `40` | Scores ≤ this are Critical |
| `high_max` | integer | `60` | Scores ≤ this (but > critical_max) are High |
| `medium_max` | integer | `80` | Scores ≤ this (but > high_max) are Medium |
| `low_max` | integer | `95` | Scores ≤ this (but > medium_max) are LowMedium |
| `healthy_min` | integer | `90` | Scores > this are Healthy (checked first) |

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
healthy_min = 90
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
| `api_base` | string | no | API base URL (defaults to provider standard); also accepts `base_url` as an alias |
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
| `compression_level` | string | `"auto"` | Compression level: `"none"` / `"light"` / `"medium"` / `"aggressive"`. Honored since 0.9.5; `"auto"` defers to `assess_large_pr()` (severity-driven) selection. See `docs/code-audit-default.toml` |
| `chunking_strategy` | string | `"adaptive"` | Chunking strategy: `"files"` / `"hunks"` / `"adaptive"` (see `src/team/orchestrator.rs`) |
| `max_chunks_per_expert` | integer | `3` | Maximum number of chunks each expert receives |
| `max_context_file_bytes` | integer | `60000` | Total byte budget for full changed-file contents injected into expert prompts (local reviews only, per-file cap 20000 bytes; `0` disables) |

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
compression_level = "auto"
chunking_strategy = "adaptive"
max_chunks_per_expert = 3
max_context_file_bytes = 60000
```

## `[languages]`

Language detection and per-language profiles (see `LanguagesConfig` / `LanguageProfile` in `src/models/config.rs`).

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `dominant` | string | `""` | When set to a non-empty language name, overrides auto-detection |
| `profiles` | table | `{}` | Per-language profiles, keyed by language name |

### `[languages.profiles.<name>]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `name` | string | `""` | Language name (e.g. `Rust`, `Python`) |
| `comment_prefixes` | string[] | `[]` | Inline comment prefixes (e.g. `["//"]` for Rust, `["#"]` for Python) |
| `doc_prefixes` | string[] | `[]` | Doc comment prefixes (e.g. `["///", "//!"]` for Rust, `["\"\"\""]` for Python) |
| `test_patterns` | string[] | `[]` | File-path patterns that indicate a test file |
| `style_configs` | string[] | `[]` | Style/linter configuration files to check for this language |
| `naming_hint` | string | `""` | Naming convention hint for LLM prompts |
| `error_hint` | string | `""` | Error-handling convention hint for LLM prompts |

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

## `[[git_platforms]]` (Web UI, persisted to `ui-state.toml`)

Git platform instances are **not** read from `.code-audit-config.toml`: they are managed in the Web UI (**Git 平台** card) and persisted to `ui-state.toml` in the config directory (default `~/.config/review-engine/ui-state.toml`, overridable via `REVIEW_UI_STATE_FILE` or `REVIEW_ENGINE_CONFIG_DIR`) as `[[git_platforms]]` entries. They are hot-effective and drive webhook verification, review-time GitLab API pulls, admin-level System Hook dispatch, and per-platform project filtering. Only `type = "gitlab"` is implemented today.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `name` | string | `""` | Unique, user-chosen instance name; the merge key for `PUT /api/v1/config` |
| `type` | string | `"gitlab"` | Platform kind; only `gitlab` is implemented |
| `base_url` | string | `""` | Instance URL as it appears in GitLab payloads (`external_url`); used to **match** inbound webhooks (Web UI field `baseUrl`) |
| `internal_base_url` | string (optional) | `""` | Container-reachable URL for review-time GitLab API pulls (Web UI field `internalBaseUrl`). Empty = fall back to `base_url`, then to the payload URL. Not part of webhook matching |
| `token` | string | `""` | GitLab API token. Encrypted at rest (`enc:` prefix) |
| `webhook_secret` | string | `""` | Legacy webhook secret (`X-Gitlab-Token` header verification). Encrypted at rest |
| `webhook_signing_secret` | string | `""` | GitLab 19+ signing token (`whsec_...`, Standard Webhooks). Encrypted at rest |
| `allowed_projects` | string[] (optional) | `[]` | `path_with_namespace` allowlist for webhook-triggered reviews (Web UI field `allowedProjects`). Empty = every project allowed; non-empty = only listed projects trigger reviews (unlisted projects' events get `200 ignored`); exact, case-sensitive matching |

Non-empty secrets are stored encrypted; on-disk values carry an `enc:` prefix and must never be hand-edited. See [`docs/configuration.md`](configuration.md) for the backup rule (`secrets.key` must be backed up with `ui-state.toml`).

```toml
# ui-state.toml (Web-UI-managed; do not hand-edit secrets)
[[git_platforms]]
name = "gitlab-main"
type = "gitlab"
base_url = "https://gitlab.example.com"
internal_base_url = ""                       # optional; empty = use base_url
token = "enc:<base64(nonce‖ciphertext‖tag)>"
webhook_secret = "enc:<...>"
webhook_signing_secret = "enc:<...>"
allowed_projects = ["group/project-a", "group/project-b"]   # empty = all projects
```

## Configuration Loading Order

1. Built-in defaults (`docs/code-audit-default.toml`) with environment overrides
2. User-level config (`~/.config/review-engine/.code-audit-config.toml`)
3. Project-level config (`.code-audit-config.toml` in the project root)
4. Environment variables (`LLM_CONFIG`, `CODE_AUDIT_COMMANDS`, etc.)
5. CLI arguments (`--llm-config`, `--config`)

When no `--config` is given, the user-level file contributes `[[llm]]` (as a fallback) and `[report]` (as global defaults); the project-level file then overrides `commands`/`review_experts` (extended) and `[report]` (replaced wholesale — fields omitted in the project file fall back to serde defaults, not user-level values).
