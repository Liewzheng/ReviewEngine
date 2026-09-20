# Configuration Schema Reference

Configuration is done via `.code-audit-config.toml` in the project root or `.code-audit-config.toml` in the state directory for user-level config (`~/.config/review-engine/` by default — see [Data directory](configuration.md#data-directory-serve---data-dir)).

## File Format

The config file uses TOML format. Below is the complete schema with all available sections.

## Top-level Fields

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `output_dir` | string | `<state dir>/reports/` (`~/.config/review-engine/reports/` unless `serve --data-dir` moved the root) | Directory for auto-saved reports |
| `max_team_size` | integer (optional) | `6` | Maximum number of experts per review. Parsed and carried in the resolved config, but **no review path enforces it today**: the pipelines build the enabled expert team directly (`AppConfig::build_expert_defs`), and the team-selection code that does read this key is not called by the CLI, `serve` or the webhook handler. A `0` is therefore never honoured either — see [Concurrency keys](#concurrency-keys-max_team_size--max_concurrent_llm_calls) |
| `max_concurrent_llm_calls` | integer (optional) | `6` | Maximum concurrent LLM API calls, enforced by the review pipelines. Read only when the file is passed as the whole configuration (`--config <file>`); a value of `0` is treated as "not decided" (the default applies) rather than as a limit |

### Concurrency keys (`max_team_size` / `max_concurrent_llm_calls`)

Whether these two top-level scalars are honoured depends on how the file is read:

| How the file is read | Are the keys honoured? |
|---|---|
| `--config <file>` (and the library's `ConfigSource::Path` / `Inline`) | Yes — the whole `AppConfig` is deserialized, so `max_concurrent_llm_calls` bounds the review's LLM concurrency. `max_team_size` reaches the config but is not enforced by any review path (see above) |
| The auto-detected `.code-audit-config.toml` (user-level `~/.config/review-engine/` or project-level, i.e. what `reng review` reads without `--config`) | **No** — the resolver lifts only `llm`, `report`, `commands` and `review_experts` out of the file, so both scalars are ignored there |
| The Web UI / database (`serve`) | The Configuration page's `advanced.maxConcurrentReviews` is stored in `review.db` and written to both `AppConfig` fields; `max_concurrent_llm_calls` is the one that actually limits concurrency |

A value of **`0` is treated as "not decided", never as a limit**: a concurrency semaphore with zero permits would leave every expert task waiting forever — a silent hang with no error, no timeout and no last log line — so the built-in default `6` applies instead. `reng` prints one warning naming the key it ignored, and the review and repo-review pipelines carry their own backstop for callers that never go through the CLI (they warn if a `0` reaches a semaphore at all).

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
| `adjudicate` | boolean | `true` | Final false-positive pass after lead consolidation: each finding at or above `adjudicate_min_severity` is re-examined by the lead model against the **full** content of the cited file (bypassing the context byte caps), and findings the code disproves are dropped with a recorded reason in `adjudicated_removed`. Server-side (webhook/API) reviews fetch that file through the provider API at the reviewed SHA — GitLab `GET /projects/:id/repository/files/:path/raw`, GitHub contents API — because they never clone; local reviews read the checkout. Fail-open: an unfetchable file (404, transport failure, or a token without repository read access) keeps the affected findings unchanged and logs why. The summary is recomputed over the surviving findings when the pass finishes (RENG-73), so the prose never counts a severity the published list lost; **downgrades** are only logged (`adjudicate_min_severity` candidates downgraded in place), never recorded in the payload |
| `adjudicate_min_severity` | string | `"high"` | Minimum severity that reaches the adjudication pass: `critical`, `high`, `medium`, `low` or `note`. Unrecognized values fall back to `high` with a warning |
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

Git platform instances are **not** read from `.code-audit-config.toml`: they are managed in the Web UI (**Git 平台** card) and persisted to `ui-state.toml` in the config directory (default `ui-state.toml` in the state directory — `~/.config/review-engine/ui-state.toml` unless `serve --data-dir` or the global `--config-dir` moved the root — overridable via `REVIEW_UI_STATE_FILE`, `REVIEW_ENGINE_CONFIG_DIR` or `--data-dir`) as `[[git_platforms]]` entries. They are hot-effective and drive webhook verification, review-time GitLab API pulls, admin-level System Hook dispatch, and per-platform project filtering. Only `type = "gitlab"` is implemented today.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `id` | string (UUID, optional) | `""` | **Stable entry identity** (RENG-96). Assigned on first save; never shown in the UI. `PUT /api/v1/config` secret-keep matches on this id first, falling back to `name` for pre-RENG-96 (id-less) payloads; a well-formed id is kept even on a cold-start replay, so it never drifts across restarts. A legacy row without one gets it on the next write without losing its credentials |
| `name` | string | `""` | Unique, user-chosen instance name; a display label and the secret-keep fallback key for id-less payloads |
| `type` | string | `"gitlab"` | Platform kind; only `gitlab` is implemented |
| `base_url` | string | `""` | Instance URL as it appears in GitLab payloads and pasted MR URLs (`external_url`); used to **match** inbound webhooks and REST `gitlab_mr` submissions (Web UI field `baseUrl`) |
| `internal_base_url` | string (optional) | `""` | Container-reachable URL for review-time GitLab API pulls (Web UI field `internalBaseUrl`). Empty = fall back to `base_url`, then to the payload/submitted URL. Not part of webhook matching; a REST `gitlab_mr` URL on this address also identifies the entry (RENG-33), and a URL whose host matches this entry's host with a different port does too when the host is unique (RENG-90) |
| `token` | string | `""` | GitLab API token. Encrypted at rest (`enc:` prefix) |
| `webhook_secret` | string | `""` | Legacy webhook secret (`X-Gitlab-Token` header verification). Encrypted at rest |
| `webhook_signing_secret` | string | `""` | GitLab 19+ signing token (`whsec_...`, Standard Webhooks). Encrypted at rest |
| `allowed_projects` | string[] (optional) | `[]` | `path_with_namespace` allowlist for webhook-triggered reviews (Web UI field `allowedProjects`). Empty = every project allowed; non-empty = only listed projects trigger reviews (unlisted projects' events get `200 ignored`); exact, case-sensitive matching |

Non-empty secrets are stored encrypted; on-disk values carry an `enc:` prefix and must never be hand-edited. See [`docs/configuration.md`](configuration.md) for the backup rule (`secrets.key` must be backed up with `ui-state.toml`).

```toml
# ui-state.toml (Web-UI-managed; do not hand-edit secrets)
[[git_platforms]]
id = "3f8a2c9e-…"                     # stable identity (RENG-96); auto-assigned, never shown in the UI
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

Later sources override earlier ones:

1. Built-in defaults (`docs/code-audit-default.toml`)
2. Environment variables (`CODE_AUDIT_COMMANDS`, `CODE_AUDIT_SCORING_ENABLED`, etc.), applied to the built-in defaults
3. User-level config (`.code-audit-config.toml` in the state directory, `~/.config/review-engine/` by default)
4. Project-level config (`.code-audit-config.toml` in the project root, or the file named by `--config`)
5. **The configuration database** (`review.db` in the config directory) — highest priority, applied **per key**: a key the database carries wins, a key it does not carry keeps the TOML value

`--config` selects the project-level file in step 4 and `--llm-config` replaces the resolved `[[llm]]` list for one run; neither is a further layer. `LLM_CONFIG` is the one surface that does not follow the order above on the CLI — there it outranks both the file and the database (see the priority note in [`[[llm]]`](#llm)). See [Configuration resolution order](configuration.md#config-resolution-order) for the full rules and how to point the CLI at a server's config directory with `--config-dir`.

When no `--config` is given, the user-level file contributes `[[llm]]` (as a fallback) and `[report]` (as global defaults); the project-level file then overrides `commands`/`review_experts` (extended) and `[report]` (replaced wholesale — fields omitted in the project file fall back to serde defaults, not user-level values).
