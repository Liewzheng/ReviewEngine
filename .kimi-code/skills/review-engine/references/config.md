# ReviewEngine Configuration Reference

ReviewEngine uses a TOML config file named `.code-audit-config.toml`. It can
live in the project root, in your user config directory
(`~/.config/review-engine/.code-audit-config.toml`), or be passed explicitly
with `--config`.

## Config resolution order

Configuration is merged from multiple sources. Later sources override earlier
ones:

1. **Embedded default** — built into the binary.
2. **Project config** — `.code-audit-config.toml` in the current working directory.
3. **User config** — `~/.config/review-engine/.code-audit-config.toml`.
4. **Environment variables** — `LLM_CONFIG`, `CODE_AUDIT_COMMANDS`, etc.
5. **CLI arguments** — `--config`, `--llm-config`, etc.

Use this to keep secrets (API keys) in your user config and share
project-specific expert settings in the repo.

## Minimal single-provider config

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
# disable_thinking = true
```

Save this as `.code-audit-config.toml` in your project or in
`~/.config/review-engine/.code-audit-config.toml`.

> `disable_thinking = true` makes ReviewEngine send
> `"thinking": {"type": "disabled"}` in the chat request. Set it for reasoning
> models (e.g. `deepseek-v4-flash`) that otherwise consume the whole
> `max_tokens` budget on `reasoning_tokens` and return an empty `content` on
> large prompts (repo-review CodeQuality chunks are the common case). Leave it
> unset for providers that do not recognise the field.

## Multi-provider fallback

If the first provider fails, ReviewEngine tries the next one in order:

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

> ReviewEngine does not expand shell variables inside TOML values. Store keys
directly in the file, or pass the whole provider block through the `LLM_CONFIG`
environment variable for dynamic values.

## Command enablement

Every command is disabled by default. Enable the ones you want under
`[commands]`:

```toml
[commands]
review = true
describe = true
improve = true
repo_review = false
update_changelog = false
```

After a command is enabled globally, individual experts decide whether they
participate via their own `commands` list.

## Using `LLM_CONFIG`

Set the whole provider configuration as JSON in an environment variable. This is
useful for CI and for keeping API keys out of files:

```bash
export LLM_CONFIG='[{"provider":"openai","model":"gpt-4o","api_key":"sk-your-key","api_base":"https://api.openai.com/v1","max_tokens":4096,"temperature":0.3}]'
```

Provider priority is:

`--llm-config` CLI > `LLM_CONFIG` env var > `[[llm]]` entries in TOML.

## Supported providers

- OpenAI (e.g., `gpt-4o`)
- Anthropic (e.g., `claude-sonnet-4-20250514`)
- DeepSeek (via OpenAI-compatible API)
- Any OpenAI-compatible provider

## Expert team basics

Experts are defined under `[review_experts.<name>]`. Key fields:

- `enabled` — whether the expert takes part.
- `weight` — influence on the overall score. All enabled experts' weights must
  sum to exactly 100.
- `commands` — list of commands this expert participates in
  (for example `["review", "repo_review"]`).
- `role`, `title`, `principles`, `focus`, `standards`, `prompt` — define the
  expert's identity and review criteria.

## Security reminder

Do not commit API keys to version control. Use the `LLM_CONFIG` environment
variable, a secrets manager, or a user-level config file outside the repository.
