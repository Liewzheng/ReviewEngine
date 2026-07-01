# Configuration

review-engine is driven by a TOML config file named `.code-audit-config.toml`. You can place it in a project root, in your user config directory, or pass a specific file with `--config`.

---

## Config resolution order

Configuration is merged from multiple sources. Later sources override earlier ones:

1. **Embedded default** — `docs/code-audit-default.toml` built into the binary.
2. **Project config** — `.code-audit-config.toml` in the current working directory.
3. **User config** — `~/.config/review-engine/.code-audit-config.toml`.
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

To print the built-in default config without prompts:

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

## Full schema

For every available field, see [`docs/config-schema.md`](config-schema.md).
