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

**Web UI layer (a running server).** `review-engine serve` also stores what the Web UI configures — LLM providers, git platforms, review rules, and per-expert `enabled` / `weight` edits — in its configuration database (`review.db` by default). On startup that stored state is applied **over** the config file: `config files / env / CLI < database`. The file stays the base/default; a stored value wins where it exists, and everything the Web UI never touched keeps its file value. See [Web UI](#web-ui).

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

A `[[llm]]` entry also accepts `disabled = true` (RENG-75): the entry keeps its configuration but is skipped by the review chain and never probed — the same switch the Web UI's provider cards expose, defaulting to `false`.

### Chain order and the primary provider

**The stored order is the chain.** The provider list order (the Web UI card order, `llm.providers[]` of `GET /api/v1/config`, persisted as each `llm_providers` row's `position`) is the order a review walks. The **first enabled** provider is the head — the primary — and the rest are fallbacks in order.

1. **Disabled providers are skipped entirely** (`disabled`, RENG-75). A disabled provider keeps its full configuration and its recorded usage history, but reviews never run on it, the health probe never checks it (`GET /api/v1/llm/providers` reports `status: "disabled"` — deliberately off, distinct from `offline` — and `chainPosition: null`), and it casts no vote on the dashboard's `overall`. Re-enabling restores it exactly as it was.
2. **The recorded primary is a compatibility echo of the head.** The persisted `llm.primaryProvider` selection still leads the chain when it names an enabled provider (moving it to the head without reshuffling the stored order); when it is empty, names no stored provider, or names a disabled one, the save pipeline normalises it to the first enabled provider (or empties it when none are enabled).

With the stored list `[xiaomi, deepseek]` and `deepseek` selected as primary, a review starts on **deepseek** and falls back to xiaomi. (Before 0.10.11 the selection was ignored at runtime and reviews always started on the first stored provider — xiaomi in this example.) The chain is produced in exactly one place, `ordered_llm_configs(primary, configs)` (`src/llm/mod.rs`), and every review-executing entry point takes its providers from it: the Web UI's review submit, repo reviews, and GitLab/GitHub webhook-triggered reviews (`AppState::ordered_llm_configs`).

- **Custom expert model** (`[review_experts.<name>] model = "…"`): that expert runs **on the chain head** with its model substituted (the head's endpoint and key, the expert's model id). This case has no fallback — the custom model is not assumed to exist on the other providers.
- **All providers disabled** (or none configured): submitting a review fails fast at enqueue time with `422` and the machine-readable code `llmAllDisabled` ("all LLM providers are disabled: re-enable one …") — a named cause, never a generic per-provider failure deep in the pipeline. With nothing configured at all the code stays `llmNotConfigured`.
- **The selection only moves when the user moves it**: setting a card as primary, adding the first provider to an empty page, and deleting the last provider are the only saves that carry `llm.primaryProvider`. An ordinary card add/edit omits it (the backend keeps the stored value), so saving from a page that is out of date — a second tab, a window opened before the change — cannot drag the selection back to the value that page last saw, and neither can deleting a *non-primary* card. Deleting the primary card itself is refused while another provider remains, naming the card and pointing at **Set as Primary**: the successor is the user's choice, never the array head picked on their behalf (RENG-72). Disabling the primary card is the one explicit action that moves the recorded selection — to the first enabled provider, since the head is always enabled.
- **Config file vs Web UI**: when the config file holds `[[llm]]` entries, CLI and webhook-triggered reviews use those in **file order** — the file is the explicit configuration for that run. The UI's primary applies to providers configured through the Web UI / database. Configure providers in one place to avoid ambiguity.
- **Order is persisted, not recomputed**: `llm_providers` rows store their list index in `raw.position`, and the loader orders by it, so the order the UI shows survives a restart. Rows without a `position` (hand-written import) sort last, in `updated_at` order. The `disabled` flag travels inside the same `raw` JSON bag, so no database migration is needed and rows written by older versions (no key) load as enabled.
- **Duplicate names and the recorded primary**: two cards may share a provider name (two accounts of one service). The recorded `llm.primaryProvider` is matched by NAME, so it always resolves to the first same-named enabled entry — which is exactly what "the head is the first enabled card" means under stored-order-is-the-chain, so no index-based echo is needed.

### Card identity: names are labels, fingerprints are identity (RENG-75)

A provider **name is a display label, not an identity** — two cards may share it (two accounts of one service, one account with two models). What uniquely identifies a card is the tuple `(provider, api_base, model, api_key)`, carried as a fingerprint:

```text
entry_fp = sha256("{provider}\n{api_base}\n{model}\n{api_key}")  →  first 12 hex chars
```

One implementation for the whole codebase (`src/llm/identity.rs`, `LLMConfig::entry_fp()`); the `\n`-joined field order and the 12-char truncation are the contract.

- **Statistics are per fingerprint.** Every recorded call (`llm_call_samples.entry_fp`, migration 0005) and every review snapshot (`reviews.llm_summary` entries carry `fp`) names the exact card; the LLM page's usage and latency numbers fold by it, so two same-named cards each report only their own history. Rotating a key or changing the URL re-fingerprints the card: its numbers start over, and the old fingerprint's rows stay in the database unattributed.
- **The fingerprint never leaves the server.** It hashes the API key, and even a truncated hash of a weak key is offline-bruteforceable — so it appears in no API response, no log line and no UI. The server matches each card to its statistics buckets internally; responses carry only the aggregated values.
- **Pre-upgrade data merges only when unambiguous.** Rows written before the fingerprint existed form an "unmarked" bucket per `(provider, model)`. The API merges that bucket into a card's numbers only when exactly **one enabled** card has that `(provider, model)`; with several enabled candidates — or none — the unmarked numbers are shown nowhere (the rows are kept in the database and still count toward the window totals).
- **Editing follows the entry, not the name.** When `PUT /api/v1/config` resolves a masked/blank API key ("leave unchanged"), it keeps the stored key of the SAME entry: the stored entry at the same array index when its `(provider, api_base, model)` matches, else the unique stored entry with that triple (a plain reorder), else nothing — two same-triple accounts are indistinguishable in a masked payload, so neither gets the other's key. Consequence: changing a card's model or URL with a masked key clears the key (re-enter it), the same rule git platforms apply to a changed baseUrl. The `disabled` flag keeps by the same entry-following rule.

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

The provider that answered is recorded per review (`reviews.llm_summary`, RENG-38) and surfaced in the Web UI: the review history shows the `provider/model` pair, the LLM page's provider cards report each provider's recorded usage over the last 7 days (使用次数 / Usages, 占比 / share of usage, 成功率 / success rate, 最近使用 / last used — RENG-56), and the 最近使用 / Recent Usage strip tags a review that used none of the chain head's provider with 未使用首选 / Primary not used. A run served by the fallback therefore never looks like a normal run, and a provider that is idle or broken shows a real, cross-checkable zero rather than a placeholder.

Call **latency** is recorded too (RENG-57): every LLM call attempt a review makes — expert calls, retries, the failed chain attempts, the lead overview, the verification and adjudication passes, the aggregator — appends one row to `llm_call_samples` (timestamp, provider, model, `latency_ms`, success/failure with the error, chain position, attempt, review id). The LLM page's card shows the resulting average over the same 7-day window (平均延迟 / Avg Latency, next to `{n} calls · {k} failed` and a 28-point latency series), while the live probe keeps its own separate value (`Probe {n} ms`, beside “Last checked”). The two numbers answer different questions — “how long do this provider's calls take” versus “is it reachable right now” — and were previously indistinguishable on screen (RENG-53).

Sampling is best-effort: a failed write is logged and never fails a review, and without a database (`REVIEW_DISABLE_DB=1`) nothing is recorded and the page shows `—`. The table is bounded by retention — samples older than **30 days** are pruned, once per review, by the write path (`src/store/llm_samples.rs`, `RETENTION_DAYS`); at the typical cost of ~10 calls per review that is a few thousand rows, and the read path only ever scans the window. There is no configuration knob for it.

### Seeing the effective order in the Web UI

Every provider card on the LLM page carries the primary badge and a quiet chain marker (链序 #1 / Chain #1). `GET /api/v1/llm/providers` returns, per provider, `position` (0-based index in the stored list), `chainPosition` (1-based rank in the runtime chain, `null` for a disabled card — it has no rank to show) and `isPrimary` (true for the chain head). 最近使用 / Recent Usage below the cards lists the newest reviews' `provider/model`, which is the ground truth for "what actually ran".

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

### Experts page (`/#/experts`)

The Experts page edits the live expert team: `GET /api/v1/system/experts` lists every expert defined under `[review_experts]` (disabled ones included, so a card can be switched back on) and `PUT /api/v1/system/experts/{id}` changes one expert's `enabled` / `weight`. Each card's switch and weight slider save themselves.

**Precedence: the database wins over the config file.** An edit made here is stored as an *override* (just the fields you changed, keyed by expert name) in the configuration database, and that override is applied over the file's `[review_experts]` on every startup and on every review dispatch — REST-submitted reviews, webhook-triggered reviews, and repo scans all run the overridden values. The config file stays the base/default:

- an override patches an expert that exists in the file; it never adds a new expert,
- a field the override does not carry keeps the file's value, so editing one expert does not freeze the others,
- removing an expert from the config file also retires its override (the orphaned entry is skipped with a debug log).

The `sum to 100` weight rule is a config-file validation (`review-engine validate`); the slider stores whatever value you set (0–100 — a higher value is rejected with `422`), exactly like editing the file by hand does not re-validate itself at runtime.

What the PUT response means:

- `"persisted": true` — the change is stored and survives a restart;
- `"persisted": false` — no configuration database is attached (`REVIEW_DISABLE_DB=1`, or an embedded instance without a data dir), so the change is **memory-only and lost on restart**. The page shows a warning notification (naming what to do: run with a data directory) instead of a success one, and the server logs a warning;
- a failed write answers `500` and the page reports the failure — a change is never silently reported as saved when it was not. The write happens **before** the change takes effect, so a `500` also means the running configuration is untouched.

A hand-edited `experts` row cannot break startup and cannot inject a value the schema forbids: a row that is not an override map, or a `weight` outside 0–100, is logged as a warning and ignored, leaving the config file's `[review_experts]` values in force.

---

## Data directory (`serve --data-dir`)

Everything the server persists lives in one directory — the **state dir**. `serve --data-dir <path>` (RENG-37) points that root somewhere else, so several isolated instances can run side by side without hacking `HOME`. The directory is created if it is missing, and the path is made absolute before it is used (it ends up in the SQLite URL and in log lines).

| Artifact | What it is | Default | With `--data-dir /srv/reng-a` |
|---|---|---|---|
| `review.db` | Embedded SQLite database (history, config, discussions) | `<state dir>/review.db` | `/srv/reng-a/review.db` |
| `secrets.key` | At-rest key for `ui-state.toml` secrets (32 bytes, `0600`) | `<state dir>/secrets.key` | `/srv/reng-a/secrets.key` |
| `ui-state.toml` | Web-UI configuration + Git credentials | `<state dir>/ui-state.toml` | `/srv/reng-a/ui-state.toml` |
| `auth.toml` | SHA-256 digest of the API token | `<state dir>/auth.toml` | `/srv/reng-a/auth.toml` |
| `dispatcher-state.json` | Webhook dedup state (SHA + content fingerprint) | `<state dir>/dispatcher-state.json` | `/srv/reng-a/dispatcher-state.json` |
| `feedback.json` | Finding feedback (`useful` / `false_positive`) | `<state dir>/feedback.json` | `/srv/reng-a/feedback.json` |
| `models-dev-cache.json` | models.dev catalog disk cache | `<state dir>/models-dev-cache.json` | `/srv/reng-a/models-dev-cache.json` |
| `logs.ndjson` | Structured log stream the Web UI reads | `<state dir>/logs.ndjson` | `/srv/reng-a/logs.ndjson` |
| `reports/` | Timestamped review reports (`report.output_dir` default) | `<state dir>/reports` | `/srv/reng-a/reports` |
| `.code-audit-config.toml` | User-level config / global `[[llm]]` fallback | `<state dir>/.code-audit-config.toml` | `/srv/reng-a/.code-audit-config.toml` |

The state dir itself is resolved in this order (first match wins):

1. `--data-dir <path>` / `REVIEW_DATA_DIR=<path>` — equivalent, the flag wins;
2. `REVIEW_ENGINE_CONFIG_DIR` — the override that predates the flag (the shipped images set it to `/app/config`);
3. `~/.config/review-engine`.

**A per-artifact variable still wins for its own artifact**, even against `--data-dir`: `REVIEW_UI_STATE_FILE` (which also moves `review.db` and `secrets.key`, they are derived from its directory), `REVIEW_AUTH_FILE`, `REVIEW_DISPATCH_STATE`, `REVIEW_FEEDBACK_PATH`, `REVIEW_MODELS_DEV_CACHE`, and `DATABASE_URL` (a PostgreSQL server instead of the embedded database). They are absolute paths written by an operator, so existing deployments that point one file at a mount keep working. They are also process-wide, so an instance started with `--data-dir` *and* one of them set is not isolated for that artifact — `serve` logs one warning per hit at startup (`<VAR> is set — <artifact> stays outside the data dir (<path>)`). Two instances that each get their own `--data-dir` and a clean environment share nothing.

Without the flag, nothing changes: the defaults are exactly the paths in the table above, and every existing env override keeps working.

### Running two isolated instances

The database, the dedup state and the log file are all per-instance, so two servers can run from one account:

```bash
# Instance A — port 8080, everything under /srv/reng-a
review-engine serve --port 8080 --data-dir /srv/reng-a

# Instance B — port 8081, a completely separate history and config
review-engine serve --port 8081 --data-dir /srv/reng-b

ls /srv/reng-a   # review.db  secrets.key  logs.ndjson  …
ls /srv/reng-b   # review.db  secrets.key  logs.ndjson  …
```

Give each instance its own `--api-token` (or bootstrap key): they have separate `auth.toml` files, so a token set on one is unknown to the other. The same works in containers — mount one volume per instance and pass `--data-dir`:

```yaml
services:
  reng-a:
    command: ["serve", "--bind", "0.0.0.0", "--port", "8080", "--data-dir", "/app/data"]
    volumes: ["./instance-a:/app/data"]
  reng-b:
    command: ["serve", "--bind", "0.0.0.0", "--port", "8080", "--data-dir", "/app/data"]
    volumes: ["./instance-b:/app/data"]
```

---

## Webhook dispatch state

Webhook-triggered reviews are deduplicated through a JSON state file the server loads at startup and rewrites atomically on every change. Per merge request / pull request it records the last reviewed **commit SHA** *and* a **fingerprint of the reviewed diff**, so an `action=update` event — or an amend / force-push, which changes the SHA while leaving the diff untouched — does not buy another full round (and does not re-post the same comments).

| Variable | Default | Purpose |
|---|---|---|
| `REVIEW_DISPATCH_STATE` | `<state dir>/dispatcher-state.json` (`~/.config/review-engine/dispatcher-state.json`, or under `serve --data-dir`) | Path of the dispatch state file. An empty value falls back to the default. |
| `REVIEW_DISPATCH_TIMEOUT_SECS` | `900` (15 min) | Age after which a `running` marker counts as stale (the review panicked, or the process restarted mid-review) and a new review may start for that MR. |

Without a state dir and without `REVIEW_DISPATCH_STATE`, the server runs without persistence: it logs a warning at startup and forgets reviewed SHAs on restart (`MrDispatcher::persistent`).

**Point this at a mounted volume inside a container.** The default resolves to `/app/.config/…`, which lives in the image layer, so *any* container recreate — `docker compose up` after editing the compose file, an image update, a container recreated on boot — wipes it and silently disarms the dedup. Both shipped compose files (`docker-compose.yml`, `deploy/standalone-compose.yml`) therefore set

```yaml
REVIEW_DISPATCH_STATE: /app/config/dispatcher-state.json
```

on the `./config` volume those files already mount for `REVIEW_ENGINE_CONFIG_DIR` (a `serve --data-dir /app/config` mount achieves the same without the extra variable). The failure mode this avoids is expensive and quiet: after one recreate, a burst of `action=update` webhooks re-reviewed seven MRs whose SHAs had not changed since days earlier, re-posting the same comments and re-billing the LLM for content already reviewed. The startup log line `Dispatcher: persisting dispatch state to <path>` names the path actually in use, which is the first thing to check when dedup appears to be off.

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
