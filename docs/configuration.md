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

The page is read-only until you click **Edit**. It is organized into cards:

- **Git 平台 (Git Platforms)** — one entry per configured Git host: name, type, Base URL, internal URL (optional), access token, Secret token / Signing token (`whsec_...`, see [GitLab webhook](integrations/gitlab.md)), and the allowed-projects allowlist. Entries are hot-effective: they drive both webhook verification and review-time GitLab API pulls without a restart.
- **Review rules** — minimum passing score (`minScore`), max review duration, block-on-critical, auto-comment-on-pass, comment template, excluded file patterns, and required experts.
- **Advanced** (collapsed by default) — log level and retention, SSE heartbeat interval, max concurrent reviews, request timeout, metrics toggle, debug mode.

LLM providers are managed on a separate **LLM page** (`/#/llm`): the primary provider and the fallback list (`[[llm]]` entries), each with API base URL, key, default model, max tokens, temperature, timeout, and retry attempts. The model dropdown auto-populates from `POST /api/v1/config/models`; **Test connection** calls `POST /api/v1/config/test` and reports success with latency or the error. Provider changes go through `POST`/`PUT`/`DELETE /api/v1/llm/providers` when you save.

### Secret handling

`GET /api/v1/config` never returns a live secret: a configured LLM API key, Git platform access token, or webhook secret comes back as the mask sentinel `***`. In read-only mode, secret fields display as dots; the reveal button only ever shows this mask, never the real value. On save:

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

The page tracks unsaved changes against a snapshot taken when you entered edit mode; the **Save** button stays disabled while nothing is dirty, and leaving the page or closing the tab with pending changes prompts for confirmation. **Cancel** discards both form edits and unsaved provider add/edit/delete operations.

`PUT /api/v1/config` is a **partial update**: the request JSON is deep-merged over the stored config, so omitted fields keep their current values. Inline validation warnings do not block saving, because empty/unchanged secret fields are interpreted as "keep the stored value" server-side.

Provider deletes deserve care: provider IDs are derived from list position (`{provider}-{index}`), so deleting an entry renumbers everything after it. The page deletes highest-index-first and re-fetches the list before applying remaining updates; if the list changed underneath, the save aborts with an error instead of updating the wrong provider. A `404` on delete is treated as success (already gone).

---

## Full schema

For every available field, see [`docs/config-schema.md`](config-schema.md).
