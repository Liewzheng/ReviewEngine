# Changelog

## [0.7.10] - 2026-07-18

### Added
- **Lead consolidation wired into the MR review path** (A1): the standard review report now carries a Lead Summary with the overall score, TL;DR, and cross-expert conflicts.
- **Full contents of changed files injected into expert prompts** (A2): the review user template gains a `## Full File Contents` section with the current contents of MR-changed files, bounded by the new `[diff] max_context_file_bytes` budget (long files truncated and noted, over-budget files listed as omitted).
- **Repo-scan REST API** (A3): new `POST /api/v1/repo-scan` and `GET /api/v1/repo-scan/{task_id}` endpoints for submitting repository scans and polling their results.
- **Remote RepoBrowser backends** (A4): the GitLab and GitHub browsers now implement `get_file` / `search_code`, so remote reviews can fetch file contents and search code without a local checkout.
- **Confidence factor and consensus multiplier in expert scoring** (A5): expert scores now combine an LLM-reported confidence factor with a cross-expert consensus multiplier, and `consensus_threshold` is wired into consolidation.
- **Webhook completion callback for review tasks** (A6): a review task may carry a callback URL (http/https, validated) that is POSTed when the task completes.
- **Reviewer Discussion rendering for expert conflicts** (A7): conflicting expert verdicts are rendered as a reviewer discussion in the report.
- **Content-pattern expert routing and semantic chunking** (A8): files are routed to experts by content patterns, large diffs are split into semantic chunks, and `compression_level` is wired.
- **Finding feedback loop** (A9): new `POST /api/v1/feedback` records verdicts on findings (by fingerprint or payload) and `GET /api/v1/feedback/stats` exposes aggregated feedback statistics.
- **Dispatcher timeout recovery and state persistence** (A10): the MR dispatcher recovers timed-out reviews and persists its state across restarts.
- **Docs**: synced `config-schema`, `rest-api`, and decision documents with the implementation.

### Fixed
- **Context-boundary prompt outdated after file-content injection** (A2 follow-up): `CONTEXT_BOUNDARY_BLOCK` in `REVIEW_SYSTEM_TEMPLATE` still claimed experts could only see the diff. It now states that experts see the diff plus the full contents of changed files when provided, that "missing X" claims may be proven from either source, and that claims about code not provided still require `note` severity, confidence ≤ 4, and an explicit `Assumption:`.
- **Resolver tests raced on shared process state**: the config resolver test module replaced its mix of a local `ENV_LOCK` and unlocked tests with a single poison-safe `fs_lock()` guard, and tests resolving config through `apply_env_overrides` now clear leaked `CODE_AUDIT_*` variables via `clear_code_audit_env()` before asserting.

## [0.7.9] - 2026-07-17

### Added
- **`repo-review --config` now takes effect**: the resolved `AppConfig` is passed through `run_repo_review_local_or_enhanced` into both `run_local_repo_review` and `run_repo_review`, populating `RepoContext.config` (previously hardcoded `None`). Language profiles from the config now reach the static and LLM repo experts, and the config is validated up front like other commands (a missing `--config` path fails with `config file not found`).
- **True concurrency in repo-review Pass 2**: the chunk-based `CodeQuality` evaluations now run as concurrent futures joined via `futures::future::join_all` (previously the loop acquired a semaphore permit and then awaited each chunk sequentially). Concurrency is bounded by a semaphore sized from `max_concurrent_llm_calls` (default 6), per-chunk failures still only log a warning without aborting the review, results are collected in chunk order, and progress updates use a completion counter so the percentage increases monotonically.
- **repo-review reuses the lead consolidator**: `CodeQuality` chunk findings are mapped to the standard `Finding` model (confidence parsed from the LLM output, defaulting to 5; the code-quality template now requests a `confidence` field) and, after the repo-specific `filter_noise`, consolidated through `ConsolidatorConfig::consolidate` (confidence downgrade/drop, deduplication, conflict detection) instead of the aggregator's `merge_deduplicate`. The `MAX_FINDINGS = 20` truncation, the cross-expert dedup pass, and the LOC-weighted chunk scoring are unchanged, and `RepoReviewOutput` is structurally unchanged. The consolidator's TL;DR generation now uses a saturating subtraction, fixing a debug-build panic when dedup/drop removed critical/high findings.
- **Verification pass supports a no-hunk mode and runs for repo-review**: when `verify_findings` is called with an empty diff-file list, the prompt carries no diff-hunk section, the file list is derived from the findings' referenced files, and the system-prompt context wording is adapted (keep/drop semantics are unchanged). `run_repo_review` now honors `[report] verification_pass` / `verification_max_file_bytes`: after Pass 2 and before aggregation, the standard code-quality findings are re-checked against the scanned files' full contents (fail-open, as in the review pipeline). Dropped findings are stripped from the chunk scores, exposed as `dropped_findings` in the JSON output (serde-compatible default), and listed in a "Dropped by verification" appendix (with the checked-count summary) at the end of the Markdown report.
- **Shared context-boundary prompt rules**: the `CONTEXT BOUNDARY` paragraph of `REVIEW_SYSTEM_TEMPLATE` is extracted into a shared `CONTEXT_BOUNDARY_BLOCK` (the rendered review template is byte-identical), and a repo-adapted `CONTEXT_BOUNDARY_BLOCK_REPO` ("you can only see the files provided to you; do not assert the content of files not provided; downgrade such claims to `note` with low confidence and an explicit `Assumption:`") is injected into both the `CODE_QUALITY` and `ARCHITECTURE_LEAD` system templates.

### Changed (breaking)
- **Crate-internal API breakage** (allowed under 0.x semver; these items are internal to the crate and not part of a stable public interface):
  - `run_repo_review` and `run_local_repo_review` each gain a required `config: Option<Arc<AppConfig>>` parameter (pass `None` to keep the previous behavior).
  - `render_repo_review_output` gains a required `verification_enabled: bool` parameter controlling the "Dropped by verification" appendix in Markdown output.
  - `experts::ScoreItem` gains a `confidence: Option<u8>` field (LLM-reported confidence 0–10; `None` defaults to 5 when mapped to `Finding`).
  - `build_output_from_aggregated` gains a `dropped_findings: Vec<DroppedFinding>` parameter and `RepoReviewOutput` gains a `dropped_findings` field (serde default keeps old JSON deserializable).

### Fixed
- **TestCoverage CI detection read files repeatedly**: `src/repo/experts/test_coverage.rs` called `std::fs::read_to_string` up to three times per candidate file (twice in the YAML-branch checks, once in the final `test` content check). The content is now read once into a local `Option<String>` and reused via `content.as_deref().map_or(false, ...)`, with the detection semantics unchanged (path hit for `.gitlab-ci.yml` / `.github/workflows/` / `Jenkinsfile`, or a YAML file whose content contains both "test" and "script"; content must contain "test" either way).

### Removed
- **Deprecated repo-scoring dead code**: deleted `src/scoring/repo.rs` (`score_repository`, `RepoScore`, `RiskItem`, scoring `ActionItem`) and the uncalled `analyze()` aggregation in `src/repo/analysis.rs` together with its only-consumer types (`RepoAnalysis`, `FileAnalysis`, `LanguageBreakdown`, `find_large_files`, `build_language_breakdown`). The still-used security-pattern scanning (`SecurityFinding`, `scan_security_patterns`) remains in `src/repo/analysis.rs`.
- **Unused repo-review prompt templates**: deleted `REPO_REVIEW_SYSTEM_TEMPLATE` / `REPO_REVIEW_USER_TEMPLATE` and `PromptEngine::build_repo_review_prompt`, which were only referenced by their own unit tests; the repo-review pipeline uses the architecture-lead and code-quality templates instead.

## [0.7.8] - 2026-07-17

### Added
- **Context-boundary rules in review prompt**: `REVIEW_SYSTEM_TEMPLATE` gains a `CONTEXT BOUNDARY` section telling experts they can only see diff fragments — not imported helper files, wrapper/helper implementations, backend route definitions, or middleware. "Missing X" claims (header, base path, validation, error handling) must be provable directly from the diff; otherwise they must be downgraded to `note` severity with confidence ≤ 4 and an explicit `Assumption:` statement in the summary, reducing false positives about code outside the diff.
- **Finding-verification pass** (opt-in): new `[report]` options `verification_pass` (default `false`) and `verification_max_file_bytes` (default `20000`). When enabled, an extra LLM pass runs after line-range validation: findings are grouped by referenced file (≤ 10 per call) and re-checked against the file's diff hunks, its current full content read from the local checkout, and the complete changed-file list. The verifier acts as a skeptical judge — it only drops findings the context directly disproves (e.g. "change not in this MR" claims refuted by the changed-file list) and keeps everything else, failing open on any LLM or parse error. Dropped findings are removed from the reports, exposed as `dropped_findings` in the JSON output, and listed in a "Dropped by verification" appendix in the Markdown report.
- **Verification-pass run summary**: the pass now always logs `Verification pass: checked N findings, dropped M` (previously silent when nothing was dropped), and the CLI Markdown "Dropped by verification" appendix renders a `_Verification pass ran: no findings were dropped (N checked)._` note when the pass is enabled but kept every finding; a non-empty appendix now ends with the checked/dropped counts, so users can tell "verification ran, all kept" apart from "verification not enabled".

### Changed
- **Diff context widened to 10 lines**: `LocalGitBrowser::get_diff` now runs `git diff -U10` (was git's default 3-line context) so experts see more surrounding code around each hunk.
- **Expert personas**: the `api` and `security` experts in the default config each gain a verifiability principle — only assert what the diff proves; when a judgment depends on code outside the diff (wrappers, route definitions, middleware), state the assumption explicitly and report it as a note with low confidence.

### Fixed
- **Stray quoted sentence in review prompt**: removed the stray double quotes around the low-confidence guidance line in `REVIEW_SYSTEM_TEMPLATE` that caused it to render verbatim (including the quotes) into the prompt.
- **`[report]` section ignored without `--config`**: when no `--config` is given, `resolve_config` now honors the `[report]` section from both the user-level (`~/.config/review-engine/.code-audit-config.toml`) and project-level (`.code-audit-config.toml`) configs. User-level `[report]` acts as global defaults; a project-level `[report]` replaces it wholesale (fields omitted there fall back to serde defaults, not user-level values). Previously `[report]` (e.g. `verification_pass = true`) only took effect via an explicit `--config <path>`.
- **Provider list lost backend `id` on load**: `loadProviders()` in the Configuration page now preserves the server-returned `id` when mapping `GET /api/v1/llm/providers` items into local entries. Previously the `id` was dropped, so deleting an existing provider never issued the backend `DELETE` request, and saving re-added every existing provider via `POST` (with an empty API key) instead of updating it via `PUT`.
- **Provider add/update endpoints dropped frontend field names**: `POST /api/v1/llm/providers` and `PUT /api/v1/llm/providers/{id}` now accept the frontend's `apiBaseUrl` and `defaultModel` field names as serde aliases for `apiBase` and `model` (the primary camelCase names are unchanged). Previously those fields were silently discarded, so `api_base`/`model` always fell back to defaults (e.g. connectivity tests hit `https://api.openai.com/v1` instead of the configured base URL).
- **Malformed provider JSON rejected with 422 plain text**: `add_provider` and `update_provider` now handle axum `Json` extractor rejections themselves and return `400 Bad Request` with the usual `{"error": ...}` JSON body, consistent with the handlers' business-logic errors.
- **Save Changes stayed disabled for provider-only changes**: the Configuration page `dirty` check now also tracks the Additional LLM Providers list (snapshot compared on load/save, pure-UI expand state excluded) and pending provider deletions. Previously it only diffed the main config JSON, so adding or removing a provider never enabled Save Changes and `saveAdditionalProviders()` was unreachable.
- **`GET /llm/providers` now returns the provider config**: each item includes `apiBaseUrl`, `defaultModel`, `maxTokens`, and `temperature` (never the API key), and `loadProviders()` prefills the edit form with these real values. Previously the form fell back to fake defaults (empty URLs, 4096/0.7) that `update_provider` would write back over the user's original configuration on save.
- **`PUT /config` no longer duplicates the primary provider on every save**: `GET /config` maps the primary provider into both the legacy `llm.*` fields and `llm.providers`, so rebuilding `llm_configs` from both sources appended one more copy of the primary per save (`openai-0` + `openai-1` duplicates). The rebuild now skips `llm.providers` entries whose provider name matches the primary, which is authoritatively expressed by the legacy fields; repeated saves are idempotent.
- **Spurious `console.error` when deleting a provider**: `saveAdditionalProviders()` runs after `PUT /config`, which may already have rebuilt the provider list without the deleted entry, so the subsequent `DELETE` returns 404. A 404 on delete is now treated as success (idempotent deletion); other errors still log.
- **Success + Error double notification on save**: the post-save flash animation called `classList.add` on `el-card` *component instances* (template refs don't resolve to DOM elements), throwing a `TypeError` that fell into the save `catch` and showed an error notification despite both write requests succeeding. The animation now reaches the DOM node via the instance's `$el` and skips cards whose element is unavailable.

## [0.7.7] - 2026-07-08

### Fixed
- **Vite dev server proxy config**: added missing `server.proxy` block to `vite.config.ts` so `/api/*` requests are correctly forwarded to the Rust backend (`http://localhost:18080`). Previously, all API calls from the SPA returned **502 Bad Gateway**, causing **LLM Status** and **Dashboard** pages to crash with `Cannot read properties of undefined (reading 'items')` and other JS errors.

### Added
- **Multi-provider CRUD API**: added `POST /api/v1/llm/providers`, `DELETE /api/v1/llm/providers/{id}`, and `PUT /api/v1/llm/providers/{id}` endpoints so users can dynamically add, remove, and update LLM providers without editing static config files.
- **Multi-provider UI in Configuration page**: new "Additional LLM Providers" card with provider type dropdown (OpenAI, Anthropic, Ollama, Google, Azure, xAI, DeepSeek, Mistral, Together AI, OpenRouter, Custom), inline expandable edit forms, and Add/Delete/Save operations.
- **Frontend provider types**: added `ProviderConfig`, `ProviderResponse`, `ProviderEntry` interfaces and `PROVIDER_TYPES` constant.

### Changed
- **UiLlmConfig**: added `providers: Vec<UiLlmProviderConfig>` field (backward-compatible with legacy single-provider fields).
- **put_config()**: now saves both the legacy single provider AND all entries from the `providers` array to `state.llm_configs`.

## [0.7.6] - 2026-07-08

### Fixed
- **Frontend navigation**: removed the broken `<Transition name="page" mode="out-in">` wrapper around `<router-view>` in `App.vue` that caused sidebar navigation (e.g., Dashboard → Config) to leave the old component rendered indefinitely.

## [0.7.5] - 2026-07-08

### Added
- **LLM model auto-discovery**: the Configuration page now fetches available models from any OpenAI-compatible endpoint (`{api_base}/models`) after the user enters an API base URL and key.
- **LLM config API**: added `POST /api/v1/config/models` to proxy model-list requests from the UI to the configured LLM provider.

### Changed
- **LLM Configuration UI**: replaced provider selector (OpenAI/Anthropic/Ollama) with a single OpenAI-compatible flow:
  - `API Base URL` field (default `https://api.openai.com/v1`)
  - `API Key` field
  - `Default Model` dropdown populated from the remote `/models` endpoint
- **Labels**: renamed `Max Tokens` to `Max Output Tokens` to clarify it controls the LLM response length.
- **Backend config mapping**: `UiLlmConfig.api_base_url` is now passed to the native `LLMConfig.api_base` field when saving, enabling custom endpoints.

### Removed
- **Anthropic support from UI**: removed the Anthropic API key field and provider option from the frontend Configuration page. The backend remains capable of using Anthropic via TOML/CLI configuration.

## [0.7.4] - 2026-07-08

### Security
- **Frontend token storage**: removed `/config.json` endpoint usage so the API token is no longer exposed as a static frontend file. The web UI now stores the token in browser `localStorage` under `review_engine_api_token` and sends it as `Authorization: Bearer <token>` on every `/api/v1/*` request.

### Fixed
- **Frontend auth in Docker**: frontend API client reads the API token from `localStorage` and sends `Authorization: Bearer <token>` on all `/api/v1/*` requests.
- **Permanent null token cache**: `getApiToken()` now reads `localStorage` on every call instead of caching a `null` value, so a token set after app startup is picked up without a reload.

### Changed
- `frontend/src/services/api.ts`: token is loaded only from `localStorage`; exports `setApiToken()` and `clearApiToken()`; no longer reads `/config.json`.
- `frontend/src/services/logs.ts`: log download now reuses the synchronous `getApiToken()` helper from `api.ts`.
- `frontend/src/App.vue`: prompts for an API token when none is stored; provides an **API Token** button to change or clear the token.
- `entrypoint.sh`: no longer writes `/app/frontend/dist/config.json`; simply execs the review-engine binary.
- `Dockerfile`: keeps `entrypoint.sh` as the entry point for future extensibility.
- `docker-compose.yml`: clarified that `REVIEW_API_TOKEN` is used for backend API authentication and must be entered into the web UI on first visit.
- `README.md`: added Docker deployment and web UI API token instructions.

## [0.7.3] - 2026-07-08

### Added
- **False-positive reduction (Phase 1)**: hardened review prompts with scope rules, confidence calibration, and diff-line interpretation to reduce speculative findings.
- **Configuration**: added `min_confidence` and `drop_low_confidence` to `ReportConfig` for configurable consolidation filtering.
- **Orchestrator**: wired the existing lead consolidator into the review pipeline; results are exposed in `TeamReport.consolidated`.
- **Evidence validation**: added `validate_findings()` in `src/output/parser.rs` to drop hallucinated findings whose file or line does not exist in the diff.

### Fixed
- **Evidence validation**: `validate_findings()` now correctly rejects pure-deletion hunks, validates both `line` and `line_end`, and handles empty diff inputs.

### Changed
- **Prompts**: `REVIEW_SYSTEM_TEMPLATE` now instructs experts to report only issues in added/modified lines and to label low-confidence findings as speculative notes.
- **Prompts**: `REVIEW_USER_TEMPLATE` now explains `+`/`-`/context lines before the diff block.
- **Docs**: added a "Reducing false positives" section to `README.md` documenting the new `[report]` config options.

## [0.7.2] - 2026-07-08

### Security
- **GitLab webhooks**: implemented Standard Webhooks signing-token verification (`webhook-signature`, `webhook-id`, `webhook-timestamp`) for GitLab 19.0+. Supports the `whsec_` key format and includes 5-minute timestamp tolerance for replay protection. Legacy `X-Gitlab-Token` secret-token verification remains supported.

### Added
- **CLI**: `--gitlab-webhook-signing-secret` flag and `GITLAB_WEBHOOK_SIGNING_SECRET` environment variable for configuring the GitLab signing token.
- **UI Config**: added `webhookSigningSecret` to the GitLab config API and the frontend Configuration page, persisted via `PUT /api/v1/config` and used as a fallback when CLI/env signing secret is not set.
- **Docs**: documented the signing-token option in `docs/integrations/gitlab.md`.

### Fixed
- **GitLab webhooks**: `verify()` now chooses the correct authentication method during migration. If a signing secret is configured and the `webhook-signature` header is present, the signature is verified; otherwise it falls back to the legacy `X-Gitlab-Token` when configured. This prevents rejecting webhooks from GitLab versions that have not yet enabled signing.
- **GitLab webhooks**: `whsec_` signing key is decoded once during handler construction instead of on every request.
- **GitLab webhooks**: replaced `chrono` timestamp comparison with `std::time` for replay-protection checks.
- **Frontend**: empty secret fields (`apiToken`, `webhookSecret`, `webhookSigningSecret`) on the Configuration page now display `(not set)` instead of a masked placeholder with a reveal button.
- **CLI**: replaced the `unwrap()` on `state.ui_config.read()` with graceful poisoned-lock handling.
- **Tests**: added coverage for multiple signatures in `webhook-signature`, timestamp tolerance boundaries, empty Standard Webhooks headers, and invalid base64 signing keys.

### Changed
- `docs/integrations/gitlab.md`: added Standard Webhooks header/replay-protection details and a note about NTP time sync.
- `frontend/src/views/Configuration.vue`: added `whsec_` prefix hint for the webhook signing secret input.

## [0.7.1] - 2026-07-08

### Added
- **Backend-Frontend Integration**: full-stack API integration between the Rust Axum backend and the Vue 3 + Element Plus frontend, including endpoints for config, experts, system health, LLM providers, logs, dashboard, queue control, and server-sent events.
- **Queue**: new `POST /queue/tasks/{id}/retry` endpoint to re-queue failed tasks from the Queue Monitor UI.
- **Docs**: documented Queue API endpoints (`/api/v1/queue/*`) in `docs/rest-api.md`.

### Fixed
- **Queue Monitor**: real retry wired to the backend; cancel-all-failed now uses `Promise.allSettled` to handle partial failures; auto-refresh guarded against overlapping requests.
- **Experts Management**: edit modal restricted to API-supported fields (`enabled`, `weight`); `name`, `category`, and `description` are now read-only.

### Changed
- Moved generated agent/subagent artifacts (test reports, plans, UX reviews, screenshots, logs, test case files) to `reports/` and ignored them in `.gitignore`.

## [0.7.0] - 2026-07-06

### Added
- **Scoring Configurability**: `PenaltyConfig` and `RiskThresholdConfig` added to `ScoringConfig` — expert penalties, risk thresholds, and consensus threshold now configurable via TOML.
- **Test Coverage**: 150+ new tests across llm/client, config/resolver, context/gather, server/auth, output/parser, scoring/review, team_renderer, lead_consolidator.
- **Security Hardening**: `MAX_DIFF_SIZE` (10 MiB), `MAX_TOML_SIZE` (1 MiB), `MAX_WEBHOOK_BODY_SIZE` (1 MiB) limits; `is_safe_diff_path` for path traversal rejection; `sanitize_user_arg` for shell metacharacter rejection; `subtle::ConstantTimeEq` for all token comparisons.
- **Config**: `[commands] review = true` in default config (was `false`, blocked new users).
- **CLI**: `--github-token` for review/improve/describe/serve; `Ask` and `UpdateChangelog` commands; `--diff`/`--local-path`/`--staged` for improve/describe.
- **Security**: `AuthConfig` production `panic!` replaced with `Result`; `RateLimiter` race condition fixed (single-lock critical section).
- **Code Quality**: `PromptEngine::try_new()` returning `Result`; tokenizer `expect()` replaced with graceful fallback; `parse_aggregator_response` fenced YAML fallback; aggregator language `zh` → `en`.

### Changed
- `docs/config-schema.md` and `docs/code-audit-default.toml` updated with new scoring options.
- Config resolution order documented correctly (User → Project → Environment → CLI).
- Removed stale `.pr-agent.toml` references from all documentation.
- Synced version numbers across docs.

## [0.6.11] - 2026-07-06

### Fixed
- **Config**: Set `[commands] review = true` in default config so `review-engine review` works out of the box.
- **Config**: Fixed `init` weight auto-allocation rounding so the total always equals 100.
- **Config**: Fixed `init` commands generation to use snake_case (`repo_review`) matching the config schema.
- **Config**: Added `validate_experts` check to `POST /api/v1/config/validate` endpoint.
- **Docs**: Corrected config resolution order (User config → Project config, not reversed).
- **Docs**: Removed stale `.pr-agent.toml` references from all documentation.
- **Docs**: Translated Chinese sections in `config-schema.md` to English.
- **Docs**: Synced version numbers: `rest-api.md` (0.4.0→0.6.11), `SKILL.md` (0.6.3→0.6.11).
- **Docs**: Fixed `enterprise.md` filename reference and `CHANGELOG.md` Unreleased dates.
- **Security**: Replaced `server/auth.rs` production `panic!` with `Result`-based error handling.
- **Security**: Fixed `RateLimiter` race condition by merging RPM/TPM check and request record into a single `lock`.
- **Security**: Used `subtle::ConstantTimeEq` for GitLab webhook token comparison (timing attack fix).
- **Security**: Used `subtle::ConstantTimeEq` for API token comparison (timing attack fix).
- **CLI**: Added `--github-token` to `review`, `improve`, `describe`, and `serve` commands.
- **CLI**: Added `Ask` and `UpdateChangelog` CLI commands.
- **CLI**: Auto-detects GitLab vs GitHub provider from URL in `run_mr`, `run_improve`, `run_describe`.
- **CLI**: Added `--diff`, `--local-path`, and `--staged` options to `improve` and `describe`.
- **CLI**: Added `--gitlab-token` and `--gitlab-webhook-secret` to `serve`.
- **CLI**: Made `--config` optional in `validate` (auto-loads default path).
- **Code Quality**: Added `PromptEngine::try_new()` returning `Result`; `new()` is a thin wrapper.
- **Code Quality**: Replaced `tokenizer` `expect()` with graceful fallback to char counting.
- **Code Quality**: Added `parse_aggregator_response` fallback (fenced YAML → empty report).
- **Code Quality**: Added `tracing::warn!` for `parse_improve_response` failure (was silent).
- **Code Quality**: Fixed `DefaultOrchestrator` command string matching (`repo_review` not `reporeview`).
- **Code Quality**: Improved error messages distinguishing "command disabled" vs "no expert matched".
- **Code Quality**: Fixed `max_tokens` default 2048 → 4096 (matching docs).
- **Code Quality**: Fixed hardcoded aggregator language `"zh"` → `"en"`.

## [0.6.10] - 2026-07-06

### Fixed
- Harden git ref/path validation in `src/context/gather.rs` to prevent argument injection via branch names, paths, and user-controlled `git log` arguments. This validation is stricter than before; ref names or paths that previously slipped through are now skipped with a warning.
- Log a warning when project context gathering fails instead of silently swallowing the error.
- Corrected README wording about lead overview fallback behavior.

## [0.6.9] - 2026-07-06

### Added
- All PRs (small and large) now run a lead overview before expert review.
- Lead overview produces a branch summary (from the PR diff and branch commits) and a project overview (from project config, README, manifest, file tree, and git logs).
- Both the branch summary and project overview are injected into every expert's prompt.

### Fixed
- Expert prompt now keeps `## Project Context` visible even when `## Lead Context` is present, so domain experts retain structured project config (`project_type`, `os`, `arch`, `domain`, `constraints`).
- `gather_project_context` no longer silently swallows git/IO errors; failures are logged with `tracing::warn!` and fall back to empty defaults.
- Replaced `String::floor_char_boundary` in `truncate_string` with a `String::get`-based implementation to avoid raising the Rust MSRV.
- Hardened git command construction in `src/context/gather.rs`: uses `current_dir` instead of `-C`, validates branch ref names, and filters user-controlled arguments to prevent argument injection.
- SVG files are no longer treated as binary when scanning the filesystem.
- Test helper `init_git_repo` now uses `git init` + `git checkout -b main` instead of `--initial-branch=main` for broader git version compatibility.

## [0.6.8] - 2026-07-03

### Added
- Added `tracing::warn!` calls for silent fallbacks in configuration parsing, describe response parsing, and diff token counting.
- Added missing doc comments for `RateLimiter`, `ProviderRegistry::from_configs`, and `clean_yaml`.
- Added a one-line design-proposal status note to `docs/professional_team_design.md` and `docs/repo_aware_review_strategy.md`.

### Changed
- Translated the Chinese section/header comments in `docs/code-audit-default.toml` to English; all TOML keys and values are unchanged.
- Made `install.sh` URL encoding portable by preferring Python and falling back to `jq` or a pure-shell implementation; the `sanitized_config_ref` signature and call sites are unchanged.

### Fixed
- Updated `notify` to 8.x and `inquire` to 0.9.4 to resolve cargo-audit unmaintained-dependency warnings (`fxhash`, `instant`).

## [0.6.7] - 2026-07-03

### Fixed
- `repo-review` scanner now respects `.gitignore` and excludes Git submodule directories by using `git ls-files` for file listing in Git repositories.
- Binary files are now skipped entirely during repo-review scans instead of being included with `is_binary: true`.

## [0.6.6] - 2026-07-03

### Added
- `ProjectConfig` gains optional project context fields: `project_type`, `os`, `arch`, `domain`, and `constraints`. These help reviewers understand the target environment and avoid irrelevant generic advice.
- Review user prompt now includes a `## Project Context` block when any project context fields are configured, populated from the `[project]` config section.

### Changed
- `REVIEW_SYSTEM_TEMPLATE` now requires every finding to include `evidence`, `impact`, `recommendation`, and `effort`, and instructs experts to fill `evidence` with the actual code snippet from the diff.
- `REVIEW_SYSTEM_TEMPLATE` now downgrades code-quality/style findings (function size, duplication, naming, etc.) to `low` or `note` unless they cause a concrete functional, performance, or security bug.

### Removed
- Removed the unused `review_context` module (`src/review_context/mod.rs`); no other source file imported it. This resolves the self-review false positive about a potential circular dependency with `team`/`scoring`.

## [0.6.5] - 2026-07-03

### Added
- `WebhookHandler` trait in `src/server/webhook.rs` for provider-agnostic webhook dispatch.

### Changed
- Renamed `tools` module to `actions` to clarify command structure.
- Split diff processor and unified filter logic in `src/diff/`.
- Grouped standalone modules and added a centralized `error` module.
- Unified `GitProvider` and `Publisher` abstractions.
- Consolidated GitHub and GitLab client implementations under `src/git_provider/`.
- `CommandRegistry` merged into `actions`; the standalone `commands` module removed.
- `src/server/router.rs` no longer depends on concrete `github`/`gitlab` modules; it accepts a `Vec<Arc<dyn WebhookHandler>>` and registers each handler via a shared closure route.
- `GitHubWebhookState` and `GitLabWebhookState` renamed to `GitHubWebhookHandler` and `GitLabWebhookHandler`; both implement `WebhookHandler` and expose `new` constructors.
- GitLab webhook token is now read from `GITLAB_TOKEN` in `src/cli/mod.rs` and passed into `GitLabWebhookHandler::new` instead of being read from the environment inside the handler.
- Documentation: clarified purpose of Python bindings, metrics, and the error module.

### Fixed
- Expert registry now includes experts configured with an empty commands list.

### Removed
- Removed top-level `github`/`gitlab` shims in favor of `git_provider`.

## [0.6.4] - 2026-07-02

### Added
- AI skill support: project-level skill files under `.kimi-code/skills/review-engine/` with command and configuration references.
- README/justfile documentation for installing and using the `review-engine` AI skill.
- Integration test suite: `tests/cli.rs` (6 tests) and `tests/server.rs` (3 tests).

### Fixed
- `init --default` now writes the built-in default config to `.code-audit-config.toml` instead of printing to stdout.
- `validate` now runs full configuration validation (`load_and_apply`) rather than only parsing TOML.
- `/metrics` endpoint always exposes at least one `review_engine_*` series by registering a `review_engine_build_info` gauge.

### Changed
- Unified orchestrator modules: removed `src/orchestrator.rs` and merged its public API into `src/team/orchestrator.rs`.
- Unified scoring modules: split MR/PR review scoring into `src/scoring/review.rs` and repository health scoring into `src/scoring/repo.rs`, with `src/scoring/mod.rs` as a thin re-export layer.

## [0.6.3] - 2026-07-02

### Fixed
- Fixed malformed markdown code fences in repo-review reports when LLM evidence already includes its own fences.
- Updated `prometheus` from 0.13.4 to 0.14.0 (with default features disabled, keeping only `process`) and `pyo3` from 0.23.5 to 0.29.0 to resolve `cargo audit` vulnerabilities in `protobuf` (RUSTSEC-2024-0437) and `pyo3` (RUSTSEC-2025-0020, RUSTSEC-2026-0177).

## [0.6.2] - 2026-06-30

### Added
- Project license switched to Apache-2.0; added `license = "Apache-2.0"` to `Cargo.toml`
- `CONTRIBUTING.md` contribution guidelines
- `THIRD_PARTY_LICENSES.md` and `scripts/generate-third-party-licenses.sh`
- `deny.toml` cargo-deny license audit configuration
- GitLab CI `cargo-deny` job in `.gitlab-ci.yml` for automatic dependency license auditing
- New user-facing documentation: `docs/getting-started.md`, `docs/configuration.md`, `docs/enterprise.md`, and `docs/integrations/*`
- Chinese README (`README.zh-CN.md`)
- `.notes/` directory for internal planning, roadmaps, and business strategy documents
- Evidence, impact, recommendation, effort fields in LLM expert findings (architecture lead + code quality)
- 11 unit tests for `convert_scores`, `pick_top_risks`, and `build_languages`
- Shared `parse_yaml_findings()` helper for consistent YAML→ScoreItem parsing
- `severity_label()` static mapping (replaced heap-allocating `to_string().to_uppercase()`)
- Tests for `render_aggregated_markdown` and severity label format for all 5 severity levels

### Changed
- Replaced `dirs` dependency with `home` to avoid MPL-2.0 transitive dependency
- Rewrote `README.md` to focus on value proposition, quick start, and enterprise positioning
- Updated `install.sh` for GitHub Releases and `raw.githubusercontent.com`
- Updated public documentation URLs from private GitLab to GitHub distribution address
- Enterprise contact email set to `isletspace@outlook.com`
- Rebased `feat/licensing-compliance` onto latest `origin/main`
- `RepoReviewOutput` restructured: `overview` → `expert_scores` + `risk_categories` + `action_items` → `conclusion` (total-part-detail architecture)
- Extracted shared helpers: `build_score_breakdown`, `build_risk_categories`, `build_action_items`, `build_languages`, `pick_top_risks` — eliminating all duplicate inline code between `build_output` / `build_output_from_aggregated`
- `convert_scores` returns named struct `ConvertedScores` instead of tuple
- `pick_top_risks` uses `select_nth_unstable_by` for O(n) partial selection
- English-only, no-emoji output across all renderers (`renderer.rs`, `team_renderer.rs`, `repo_review.rs`)
- Languages list truncated to top 3 by file count
- Score breakdown `weighted_contrib` normalized by actual total weight (not hardcoded 100)
- LLM prompt templates (`ARCHITECTURE_LEAD_SYSTEM_TEMPLATE`, `CODE_QUALITY_SYSTEM_TEMPLATE`) moved from inline `format!()` to `templates.rs` constants
- Risk level mapping unified: `score_to_risk_level()` canonical function (0-40 critical, 41-60 high, 61-80 medium, 81-90 low, 91-100 healthy)
- Scoring consolidated: `compute_weighted()` shared by both `experts::weighted_total()` and `scoring::weighted_overall_score()`
- `convert_scores()` helper extracted, eliminating 65 lines of duplicate code between `build_output` / `build_output_from_aggregated`
- `RiskCategory.risk_icon` field removed (was unused duplicate of `risk_level`)

### Fixed
- Pre-existing syntax error (extra `fi`) in `install.sh`
- `install.sh` now falls back to `shasum -a 256` on macOS
- `merge_deduplicate` now merges (not drops) duplicate findings: highest severity, longest evidence/impact/recommendation, highest effort win
- Noise filtering checks both `message` and `evidence`; empty findings filtered; logs discarded count
- `top_risks` empty shows "Analysis incomplete" not "No issues found" when no expert data
- Empty message guarded in `render_detail`
- Finding heading level fixed (`###` → `####`)
- Summary heading uses `####` instead of `###` to avoid heading level jump
- Dead variable `all_details` removed from `build_output`

## [0.6.0] - 2026-06-29

### Added
- `review-engine init` — scan project and generate tailored `.code-audit-config.toml`
- Language profile system (`src/language/mod.rs`) — per-file language-aware expert evaluation
- 8 built-in language profiles: Rust, Python, C, C++, Java, JavaScript/TypeScript, Go
- Graduated large-file scoring (1pt per 100 excess LOC, cap 40)
- Language-specific CodeQuality LLM prompt hints (naming/error conventions)
- `base_url` alias for `api_base` in `LLMConfig`

### Changed
- Config resolution: removed `.pr-agent.toml` legacy support
- Documentation expert now counts `#` comments for Python, `//` for Rust per file
- CodeStyle expert detects tools for all languages present (ruff, black, rustfmt, clang-format)
- CodeQuality LLM expert reads all source files (no Rust-only filter)
- Server architecture: extracted `state.rs`, `router.rs`, `routes/` submodule
- Unified scoring interface: `Scorable` trait + `Score` struct

### Fixed
- UTF-8 truncation panic in `diff/processor.rs` (`floor_char_boundary`)
- CI detection for `.gitlab-ci.yml` path prefix (`contains` vs `ends_with`)
- Missing `"No code content"` warnings — added `tracing::warn!` on file read failures

## [0.2.1] - 2026-06-25

### Changed
- Switched from `native-tls` (openssl-sys) to `rustls-tls` for reqwest, removing cross-compile OpenSSL sysroot dependency
- CI: switched to USTC cargo mirror (`sparse+https://mirrors.ustc.edu.cn/crates.io-index/`) for faster dependency resolution
- CI: renamed `build-linux` → `build-linux-aarch64` to remove job reference ambiguity
- CI: test job now passes `--features cli` consistently

### Fixed
- Test inner items compilation error on Rust 1.96.0 (`unnameable_test_items`)
- Various unused import / unused variable warnings causing CI test failures with `-D warnings`
- `install.sh`: removed invalid `"n"` flag from `jq test()` call

## [0.4.0] - 2026-06-26

### Added
- REST API layer: async task queue, review CRUD endpoints (`POST/GET/DELETE /api/v1/reviews`)
- Config endpoints: `GET /api/v1/config`, `GET /api/v1/config/schema` (JSON Schema), `POST /api/v1/config/validate`
- System endpoints: `GET /api/v1/system/experts`, `GET /api/v1/system/version`
- SSE endpoint: `GET /api/v1/events` for real-time task status updates
- API authentication: auto-enforces token when binding `0.0.0.0`, no-auth on `127.0.0.1`
- `review-engine generate-token` — cryptographically secure random API token
- `review-engine serve --bind <addr> --api-token <token>` — server address and auth flags
- CORS support via `tower-http::cors`

### Changed
- `review-engine serve` now accepts `--bind` and `--api-token` arguments
- Upgraded from 0.3.1 to 0.4.0

### Dependencies
- `tower-http` (cors), `schemars` (derive), `tokio-stream` (sync), `rand`, `hex`

## [0.4.2] - 2026-06-27

### Added
- GitHub support: REST API client, GitProvider impl, Publisher impl, webhook handler
- HMAC-SHA256 webhook signature verification (constant-time via `hmac::verify_slice`)
- Suggestion block helper (`format_suggestion_block` for GitLab ````suggestion`)
- `MrDispatcher` unit tests (16 tests covering state machine, concurrency, boundaries)
- `--publish` flag for CLI: auto-publish review results to MR/PR discussion
- `improve` CLI subcommand: generate code improvement suggestions
- `describe` CLI subcommand: generate PR description / summary
- Shared `publish_review()` helper in `lib.rs` (eliminates duplicate publish logic in webhook handlers)

### Fixed
- GitHub webhook: empty diff path now releases dispatcher lock
- GitHub publisher: `find_or_update_discussion` uses correct PR review API (not inline comments)
- GitHub publisher: `update_discussion` calls `update_pr_review` instead of `update_review_comment`
- GitHub inline comments: added `commit_id` from PR head SHA
- GitHub webhook token: `--github-token` CLI arg now properly propagated to webhook state
- HMAC `verify_signature` changed from non-constant-time `==` to constant-time `verify_slice`
- `Cell<Option<String>>` → `Arc<Mutex<Option<String>>` for thread safety

### Changed
- Upgraded from 0.4.1 to 0.4.2

## [0.4.1] - 2026-06-27

### Added
- MR webhook dispatch dedup (`MrDispatcher`): 同一 MR 的并发 push 只触发一次 review
- Comment find-or-update: bot 更新已有 discussion 而非每次创建新评论
- `Publisher::find_or_update_discussion` trait 方法
- GitLab `list_discussions` API 支持
- API token 认证作者校验（`get_current_user_id` + `NoteAuthor`）
- MrDispatcher 单元测试（16 个测试覆盖状态机/并发/边界）

### Fixed
- `wait()` 竞态条件：`Notify` → `watch` 通道，消除错过通知永久阻塞
- InProgress 等待后未重新 try_start，新 commit 被忽略
- Note hook `/review` 未集成 dispatcher，无去重
- `get_current_user_id` 使用了错误的 project-scoped URL
- spawn 任务 error 路径未释放 dispatcher running 锁
- `find_or_update_discussion` 只检查第一条 note
- Note hook 使用 timestamp 作为去重 key，同一秒内重复 /review 被误判
- `get_json` 使用 `PRIVATE-TOKEN` 而非 `Authorization: Bearer`（认证方式不一致）
- `list_discussions` 未分页（添加 `?per_page=100`）

### Changed
- Upgraded from 0.4.0 to 0.4.1

## [0.3.0] - 2026-06-26

### Added
- `install.sh` — One-curl installer for Linux/macOS
- Automated daily/stable release pipeline using GitLab Generic Package Registry
- `install.sh` binary install with `--daily-built` and `--source`
- Cross-platform builds (aarch64 Linux, x86_64 Linux, x86_64 Windows)
- SHA256 checksum generation and verification
- `--version` / `-V` flag
- `review-engine serve` — Health check HTTP server (`/health`, `/health/ready`)
- Structured JSON logging via `REVIEW_LOG_FORMAT=json`
- `request_id` tracing per review
- User-level config fallback (`~/.config/review-engine/`)
- LLM config from `[[llm]]` TOML section (falls back from CLI args and env var)
- Default output directory `~/.config/review-engine/reports/`
- `output_dir` config option
- Describe / Improve / Ask / Repo-review / Update-changelog tool commands
- Tokenizer module (`tiktoken-rs` integration)
- Diff chunker (file-level, hunk-level, adaptive)
- Binary/vendor file filtering, language-based sorting, line truncation
- LLM provider abstraction (OpenAI, Anthropic, OpenAI-compatible)
- Context assembly module (commit messages, ticket links, language stats)
- Risk levels and scoring module (expert_score, weighted_overall_score)
- Team report renderer (overall score, risk level, expert score table)
- Lead consolidator (confidence filtering, dedup, conflict detection)
- `max_team_size` and `max_concurrent_llm_calls` config
- `deny_unknown_fields` on ExpertTomlDef for config validation
- `dirs` crate for platform-specific config paths
- `axum` dependency for HTTP server

### Changed
- `LLMClient::complete` and `complete_with_fallback` return `CompletionResult` instead of `(String, u64)`
- `apply_token_budget` uses token counting instead of character counting
- Config resolution order: project config > legacy .pr-agent.toml > user-level ~/.config/review-engine/ > embedded defaults > env overrides > CLI args
- New commit messages include structured format

### Fixed
- Lockfile detection now matches subdirectory paths (e.g., `backend/Cargo.lock`)
- SVGs no longer classified as binary files
- Cross-platform path handling in `get_related_tests`
- Health check server binds to `127.0.0.1` by default
- `/health/ready` no longer calls LLM API on every probe
- Error messages sanitized in health check responses
- `compress_deletions` uses correct path (`old_path`) for deleted files
- Chunk oversized hunks properly

## [0.2.0] - 2026-06-22

### Breaking Changes
- Renamed default config from `pr-agent-default.toml` to `docs/code-audit-default.toml`
- Renamed config struct from `PrAgentToml` to `AppConfig`
- Renamed TOML field `perspective` to `trigger_prompt` (old name still works via alias)
- Renamed CLI config option from `.pr-agent.toml` to `.code-audit-config.toml`
- Expanded `Finding` struct with new fields (`confidence`, `evidence`, `impact`, `recommendation`, `effort`, `expert_name`, `expert_role`, `references`, etc.)
- Old `detail` field replaced by `summary` (with backward-compat fallback)

### Features
- Added `--local-path`, `--base`, `--head`, `--staged`, `--since`, `--until` CLI flags for local repository review
- Added `validate` and `default` subcommands for config validation
- Added `ScoringConfig` with `enabled`, `display_individual_scores`, `display_weighted_score`
- Added `Command` enum and `CommandRegistry` for command routing
- Added `commands` field to config for enabling/disabling commands per-expert
- Added `RepoBrowser` trait for repository-aware review
- Added `ReviewInput` enum for unified input abstraction

### Improvements
- Added `build_expert_defs()` to eliminate duplicated ExpertDef construction (3 sites → 1)
- Extracted `merge_hashmap` helper to reduce config merge boilerplate
- Extracted `load_and_apply` pipeline in config resolution
- Expanded `ExpertDef`/`ExpertTomlDef` with `title`, `role`, `style`, `principles`, `focus`, `standards`, `weight`, `commands`
- Added `Severity` and `Effort` enums
