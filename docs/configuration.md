# Configuration

review-engine is driven by a TOML config file named `.code-audit-config.toml`. You can place it in a project root, in your user config directory, or pass a specific file with `--config`.

---

## Config resolution order

Configuration is merged from multiple sources. Later sources override earlier ones:

1. **Embedded default** — `docs/code-audit-default.toml` built into the binary (plus environment overrides for built-in values).
2. **User-level config** — `~/.config/review-engine/.code-audit-config.toml`.
3. **Project-level config** — `.code-audit-config.toml` in the current working directory.
4. **Environment variables** — `LLM_CONFIG`, `CODE_AUDIT_COMMANDS`, etc.
5. **CLI arguments** — `--config`, `--llm-config`, etc.

Use this to keep secrets (API keys) in your user config and share project-specific expert settings in the repo.

---

## Minimal config

A one-provider setup that enables the `review` command:

```toml
[commands]
review = true

[[llm]]
provider = "openai"
model = "gpt-4o"
api_key = "sk-your-key"
api_base = "https://api.openai.com/v1"
max_tokens = 4096
temperature = 0.3
```

Save this as `.code-audit-config.toml` in your project or in `~/.config/review-engine/.code-audit-config.toml`.

---

## Multi-provider fallback

If the first provider fails, review-engine tries the next one in order:

```toml
[[llm]]
provider = "openai"
model = "gpt-4o"
api_key = "sk-your-openai-key"
api_base = "https://api.openai.com/v1"
max_tokens = 4096
temperature = 0.3

[[llm]]
provider = "anthropic"
model = "claude-sonnet-4-20250514"
api_key = "sk-your-anthropic-key"
api_base = "https://api.anthropic.com"
max_tokens = 4096
temperature = 0.3
```

> review-engine does not expand shell variables inside TOML values. Store keys directly in the file, or pass the whole provider block through the `LLM_CONFIG` environment variable for dynamic values.

### Chain order and the primary provider

The order the runtime walks is the **authoritative chain**:

1. **The primary provider first.** In the Web UI (`/#/llm`) that is the card marked 主 / Primary — the persisted `llm.primaryProvider` selection. A review always starts there, whatever position that provider has in the provider list.
2. **Then the remaining providers in their stored order** — the card order (`llm.providers[]` of `GET /api/v1/config`, persisted as each `llm_providers` row's `position`). Selecting a primary never reshuffles that order; it only moves the selected provider to the head of the chain.

With the stored list `[xiaomi, deepseek]` and `deepseek` selected as primary, a review starts on **deepseek** and falls back to xiaomi. (Before 0.10.11 the selection was ignored at runtime and reviews always started on the first stored provider — xiaomi in this example.) The chain is produced in exactly one place, `ordered_llm_configs(primary, configs)` (`src/llm/mod.rs`), and every review-executing entry point takes its providers from it: the Web UI's review submit, repo reviews, and GitLab/GitHub webhook-triggered reviews (`AppState::ordered_llm_configs`).

- **Custom expert model** (`[review_experts.<name>] model = "…"`): that expert runs **on the primary provider** with its model substituted (primary's endpoint and key, the expert's model id). This case has no fallback — the custom model is not assumed to exist on the other providers.
- **No primary selected**, or a selection naming a provider that no longer exists: the first stored provider is the effective primary. The stored order *is* the chain, exactly like a `[[llm]]` list.
- **Config file vs Web UI**: when the config file holds `[[llm]]` entries, CLI and webhook-triggered reviews use those in **file order** — the file is the explicit configuration for that run. The UI's primary applies to providers configured through the Web UI / database. Configure providers in one place to avoid ambiguity.
- **Order is persisted, not recomputed**: `llm_providers` rows store their list index in `raw.position`, and the loader orders by it, so the order the UI shows survives a restart. Rows without a `position` (hand-written import) sort last, in `updated_at` order.

### What happens on failure

The retry decision comes from the **HTTP status** of the failure, not from the wording of the error message (RENG-35):

- **408 (request timeout), 429 (rate limit) and every 5xx** are retried — up to 3 attempts per provider, with exponential backoff and jitter.
- **Every other 4xx is permanent** and gives up after a single attempt: no second request, no backoff sleep. That is the credential case (401 wrong or revoked key, 403 key without access) and the request case (400 bad body, 404 unknown model) — re-sending the identical request cannot change either answer.
- **A failure with no HTTP status** (connection refused, DNS, TLS, timeout, unparsable response) keeps the historical retry: it is usually a transient network problem.

A permanent failure is logged with its status and attempt count, and the chain then advances immediately:

```text
LLM request failed permanently (401), not retrying
  provider=deepseek model=deepseek-chat status=401 attempt=1 max_retries=3
```

The chain advance is logged at INFO with the failed provider/model, the reason, and the next provider; when a later entry answers, a second INFO line names both the primary that did not answer and the provider that served the call:

```text
LLM fallback engaged: the primary provider did not answer, this call was served by a later chain entry
  primary_provider=deepseek used_provider=xiaomi used_model=mimo-v2.5 chain_position=2
```

A permanent verdict ends only that config's attempts, not the chain: the next entry is a different provider with its own credentials, which is the case the fallback exists for. So a wrong key on the primary costs one request and then runs on the next provider that works, and a run where *every* key is wrong costs one request per provider instead of three.

Search the log stream for `LLM fallback engaged` to see whether reviews are actually running on the fallback chain — that is the signature of an unreachable or misconfigured primary.

The provider that answered is recorded per review (`reviews.llm_summary`, RENG-38) and surfaced in the Web UI: the review history shows the `provider/model` pair, and the LLM page's 最近使用 / Recent Usage strip tags a review that used none of the chain head's provider with 未使用首选 / Primary not used. A run served by the fallback therefore never looks like a normal run.

### Seeing the effective order in the Web UI

Every provider card on the LLM page carries the primary badge and a quiet chain marker (链序 #1 / Chain #1). `GET /api/v1/llm/providers` returns, per provider, `position` (0-based index in the stored list), `chainPosition` (1-based rank in the runtime chain) and `isPrimary` (true for the chain head). 最近使用 / Recent Usage below the cards lists the newest reviews' `provider/model`, which is the ground truth for "what actually ran".

---

## Command enablement

Every command is disabled by default. Enable the ones you want under `[commands]`:

```toml
[commands]
review = true
describe = true
improve = true
repo_review = false
update_changelog = false
```

After a command is enabled globally, individual experts decide whether they participate via their own `commands` list.

---

## Expert team basics

Experts are defined under `[review_experts.<name>]`. The key rules are:

- `enabled` — whether the expert takes part.
- `weight` — influence on the overall score. **All enabled experts' weights must sum to exactly 100.**
- `commands` — list of commands this expert participates in (for example `["review", "repo_review"]`).
- `role` / `title` / `principles` / `focus` / `standards` / `prompt` — define the expert's identity and review criteria.

A small custom team might look like this:

```toml
[commands]
review = true

[review_experts.lead]
enabled = true
weight = 30
commands = ["review", "describe"]
title = "Staff Engineer"
role = "Lead Reviewer"
prompt = "You are the Lead Reviewer..."

[review_experts.security]
enabled = true
weight = 40
commands = ["review"]
title = "Security Lead"
role = "Security Lead"
prompt = "You are the Security Lead..."

[review_experts.quality]
enabled = true
weight = 30
commands = ["review"]
title = "Quality Lead"
role = "Quality Lead"
prompt = "You are the Quality Lead..."
```

30 + 40 + 30 = 100, so validation passes.

---

## Generate a starter config

The `init` command interactively creates a `.code-audit-config.toml` for the current project:

```bash
review-engine init
```

To write the built-in default config to `.code-audit-config.toml` without prompts:

```bash
review-engine init --default
```

---

## Validate a config file

Check that a config parses correctly and that expert weights sum to 100:

```bash
review-engine validate --config .code-audit-config.toml
```

A successful validation prints the number of defined experts:

```text
✓ Valid config: 6 experts defined
```

---

## Web UI

A running server (`review-engine serve`, default port 8080) also exposes a browser-based configuration page at `/#/config` (hash routing, so no server-side URL rewriting is needed). It edits the live server configuration through `GET`/`PUT /api/v1/config` — it does not write a TOML file. For the API token / bootstrap-key login flow, see [FAQ / Troubleshooting](faq.md).

The page has no edit mode — every field is editable and saves itself (see [Editing and saving](#editing-and-saving)). It is organized into cards:

- **Git 平台 (Git Platforms)** — one entry per configured Git host: name, type, Base URL, internal URL (optional), access token, Secret token / Signing token (`whsec_...`, see [GitLab webhook](integrations/gitlab.md)), and the allowed-projects allowlist. Entries are hot-effective: they drive both webhook verification and review-time GitLab API pulls without a restart.
- **Review rules** — minimum passing score (`minScore`), max review duration, block-on-critical, auto-comment-on-pass, comment template, excluded file patterns, and required experts.
- **Advanced** (collapsed by default) — log level and retention, SSE heartbeat interval, max concurrent reviews, request timeout, metrics toggle, debug mode.

LLM providers are managed on a separate **LLM page** (`/#/llm`): the primary provider and the fallback list (`[[llm]]` entries), each with API base URL, key, default model, max tokens, temperature, timeout, and retry attempts. The model dropdown auto-populates from `POST /api/v1/config/models`; **Test connection** calls `POST /api/v1/config/test` and reports success with latency or the error. Provider changes go through `POST`/`PUT`/`DELETE /api/v1/llm/providers` when you save.

The status each card shows is **probed, not inferred** (0.10.18, RENG-36): the server runs the same `GET {apiBase}/models` check as Test Connection for every configured provider and caches the verdict for 60 s. Fixing or breaking a key therefore changes the badge on the next refresh — editing credentials (or the API base, or removing the provider) drops that provider's cached health immediately, so a key broken in the UI while the service keeps running no longer leaves the card reporting 正常 / Healthy (`llm.status.healthy`) while reviews fail with 401. Only the changed provider is invalidated and re-probed; the others keep their status. A provider with no key is 离线 / Offline (`llm.status.offline`) and is never probed.

### Secret handling

`GET /api/v1/config` never returns a live secret: a configured LLM API key, Git platform access token, or webhook secret comes back as the mask sentinel `***`. A secret field therefore shows either what you are typing or that mask — the reveal control only toggles the draft in the input, never a stored value. On save:

- `***` (or leaving the field blank for LLM keys) means **keep the stored value**;
- a real value replaces the stored secret;
- an empty Git platform access token explicitly **clears** the token.

A token set via `--gitlab-token` / `GITLAB_TOKEN` at startup (or a webhook secret via `--gitlab-webhook-secret`, `--gitlab-webhook-signing-secret`, or the `GITLAB_WEBHOOK_*` variables) also appears as `***`, so an unrelated UI save cannot silently wipe it.

#### Secrets are encrypted at rest

Secrets saved through the Web UI (git platform tokens, webhook secrets, legacy GitLab credentials) are **encrypted at rest** in `ui-state.toml`: the on-disk value carries an `enc:` prefix (ChaCha20-Poly1305, fresh random nonce per value). The master key lives in a separate `secrets.key` file (32 random bytes, mode `0600`) next to `ui-state.toml` and is auto-generated on the first save.

- **Back up `secrets.key` together with `ui-state.toml`.** Losing the key makes stored secrets unrecoverable; after a loss, re-enter the secrets in the Web UI (which writes new encrypted values under a regenerated key).
- **Old plaintext files migrate automatically**: a `ui-state.toml` written by an earlier version loads fine, and the next save encrypts everything.
- Encryption covers Git credentials only; LLM API keys remain plaintext at rest.

#### env / CLI GitLab credentials are fallback-only

`GITLAB_TOKEN`, `GITLAB_WEBHOOK_SECRET`, `GITLAB_WEBHOOK_SIGNING_SECRET` and the corresponding `--gitlab-*` flags are **deprecated to fallback-only**: they take effect only when the persisted UI state holds no value for that field, and each such use logs a deprecation warning. The Web UI (`ui-state.toml`) is the authoritative source for webhook verification and review credentials and is hot-effective.

### Editing and saving

There is no edit mode: every field is editable as soon as the page loads and each edit saves itself through a debounced (500 ms) `PUT /api/v1/config`. The header carries a transient save indicator (saving / saved / failed) instead of Save and Cancel buttons; a failed save keeps the edit in the form and the form dirty, so the next edit retries it. The page tracks changes against the last persisted snapshot and skips applying a polled config while the form is dirty or a save is in flight, so a background tick cannot clobber an in-progress edit.

`PUT /api/v1/config` is a **partial update**: the request JSON is deep-merged over the stored config, so omitted fields keep their current values. Inline validation warnings do not block saving, because empty/unchanged secret fields are interpreted as "keep the stored value" server-side.

Provider deletes deserve care: provider IDs are derived from list position (`{provider}-{index}`), so deleting an entry renumbers everything after it. The page deletes highest-index-first and re-fetches the list before applying remaining updates; if the list changed underneath, the save aborts with an error instead of updating the wrong provider. A `404` on delete is treated as success (already gone).

---

## Webhook dispatch state

Webhook-triggered reviews are deduplicated through a JSON state file the server loads at startup and rewrites atomically on every change. Per merge request / pull request it records the last reviewed **commit SHA** *and* a **fingerprint of the reviewed diff**, so an `action=update` event — or an amend / force-push, which changes the SHA while leaving the diff untouched — does not buy another full round (and does not re-post the same comments).

| Variable | Default | Purpose |
|---|---|---|
| `REVIEW_DISPATCH_STATE` | `~/.config/review-engine/dispatcher-state.json` | Path of the dispatch state file. An empty value falls back to the default. |
| `REVIEW_DISPATCH_TIMEOUT_SECS` | `900` (15 min) | Age after which a `running` marker counts as stale (the review panicked, or the process restarted mid-review) and a new review may start for that MR. |

Without a home directory and without `REVIEW_DISPATCH_STATE`, the server runs without persistence: it logs a warning at startup and forgets reviewed SHAs on restart (`MrDispatcher::persistent`).

**Point this at a mounted volume inside a container.** The default resolves to `/app/.config/…`, which lives in the image layer, so *any* container recreate — `docker compose up` after editing the compose file, an image update, a container recreated on boot — wipes it and silently disarms the dedup. Both shipped compose files (`docker-compose.yml`, `deploy/standalone-compose.yml`) therefore set

```yaml
REVIEW_DISPATCH_STATE: /app/config/dispatcher-state.json
```

on the `./config` volume those files already mount for `REVIEW_ENGINE_CONFIG_DIR`. The failure mode this avoids is expensive and quiet: after one recreate, a burst of `action=update` webhooks re-reviewed seven MRs whose SHAs had not changed since days earlier, re-posting the same comments and re-billing the LLM for content already reviewed. The startup log line `Dispatcher: persisting dispatch state to <path>` names the path actually in use, which is the first thing to check when dedup appears to be off.

---

## Inline-note delivery policy

Which review findings are posted as **inline** notes (as opposed to being listed only in the `# CodeReview Board` comment) is decided by `PublishPolicy` (`src/publisher/policy.rs`). It is constructed in code with the defaults below — there is no configuration section for it, and the only override surface is these four environment variables, read once per publish:

| Variable | Default | Meaning |
|---|---|---|
| `REVIEW_PUBLISH_MIN_SEVERITY` | `high` | Lowest severity that may be posted inline: `critical` \| `high` \| `medium` \| `low` \| `note`. |
| `REVIEW_PUBLISH_MIN_CONFIDENCE` | `8` | Lowest confidence (0–10) that may be posted inline. `0` disables the floor. |
| `REVIEW_PUBLISH_MAX_INLINE_NOTES` | `2` | Maximum inline notes per round; the remainder is rolled up into the board. `0` posts none inline. |
| `REVIEW_PUBLISH_INLINE_ON_DOCS_ONLY` | `false` | Set to `1`/`true` to keep posting inline notes for a documentation/CI-only change, which is summary-only by default. |

Two further rules are not configurable and are always on: a finding must carry a **non-empty recommendation** (the corpus' out-of-scope notes were asks with no actionable content), and its line must be **inside the reviewed diff**.

A malformed value is ignored with a warning and the default is kept — a typo can never silently turn inline notes off. The defaults and the effective policy are logged once per publish: `Inline-note delivery policy: severity >= high, confidence >= 8/10, 2 inline note(s) per round, an actionable recommendation, docs/CI-only changes summary-only`.

A change set is **documentation/CI-only** when *every* changed file is documentation or CI configuration: `.md`/`.mdx`/`.rst`/`.adoc`/`.asciidoc`, anything under `docs/`, `doc/`, `documentation/`, `man/`, license/notice files (`LICENSE`, `LICENCE`, `COPYING`, `NOTICE`, `AUTHORS`, `CONTRIBUTORS`, also with a suffix such as `LICENSE-MIT`), anything under `.github/`, `.gitlab/`, `.circleci/`, `.buildkite/`, `.woodpecker/`, `.travis/`, `.ci/`, `ci/`, and the CI files `.gitlab-ci.yml`, `.gitlab-ci.yaml`, `.travis.yml`, `azure-pipelines.yml`, `appveyor.yml`, `.drone.yml`, `buildkite.yml`, `.woodpecker.yml`, `codecov.yml`, `Jenkinsfile`. `config.toml` is *not* documentation — shipped configuration is reviewed as code. An unknown change set (the diff was unavailable) is never downgraded.

Findings the policy withholds are never dropped: they stay in the board, and the board's `## Inline notes — delivery policy` section names the ones that were admitted but rolled up behind the cap. See the [GitLab](integrations/gitlab.md#inline-note-delivery-policy) / [GitHub](integrations/github.md#inline-note-delivery-policy) integration pages for the end-to-end behaviour.

---

## Full schema

For every available field, see [`docs/config-schema.md`](config-schema.md).
