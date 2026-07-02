# Changelog

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

## Unreleased

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
