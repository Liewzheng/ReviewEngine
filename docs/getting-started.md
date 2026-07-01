# Getting Started with review-engine

This guide walks you through installing review-engine and running your first review.

---

## Install review-engine

The easiest way to install review-engine is with the `install.sh` script. It downloads a single static binary and places it in `~/.local/bin`.

### Stable release (recommended)

```bash
curl -fsSL https://raw.githubusercontent.com/Liewzheng/Review-Engine/master/install.sh | bash
```

The script detects your platform, resolves the latest stable release, verifies the SHA256 checksum, and copies the default config to `~/.config/review-engine/.code-audit-config.toml`.

### Source build

If you prefer to build from source, or a binary is not available for your platform:

```bash
curl -fsSL https://raw.githubusercontent.com/Liewzheng/Review-Engine/master/install.sh | bash -s -- --source
```

This requires `git` and `cargo`.

### Daily / pre-release builds

To install a specific version (for example a daily or pre-release tag), set `REVIEW_ENGINE_VERSION`:

```bash
export REVIEW_ENGINE_VERSION="v0.x.x"
curl -fsSL https://raw.githubusercontent.com/Liewzheng/Review-Engine/master/install.sh | bash
```

> If `~/.local/bin` is not in your `PATH`, add `export PATH="$HOME/.local/bin:$PATH"` to your shell profile.

---

## Configure your LLM provider

review-engine reads providers from a TOML config file or the `LLM_CONFIG` environment variable.

Create a user-level config:

```bash
mkdir -p ~/.config/review-engine
cat > ~/.config/review-engine/.code-audit-config.toml <<'EOF'
[commands]
review = true

[[llm]]
provider = "openai"
model = "gpt-4o"
api_key = "sk-your-key"
api_base = "https://api.openai.com/v1"
max_tokens = 4096
temperature = 0.3
EOF
```

Or use an environment variable for the LLM config:

```bash
export LLM_CONFIG='[{"provider":"openai","model":"gpt-4o","api_key":"sk-your-key","api_base":"https://api.openai.com/v1","max_tokens":4096,"temperature":0.3}]'
```

Supported providers include OpenAI, Anthropic, DeepSeek, and any OpenAI-compatible API.

See [`configuration.md`](configuration.md) for multi-provider setups, expert teams, and the full config schema.

---

## Run your first local review

Review the current checkout against `main`:

```bash
review-engine review --local-path . --base main
```

Review only staged changes:

```bash
review-engine review --local-path . --staged
```

Review a commit range:

```bash
review-engine review --local-path . --since HEAD~3 --until HEAD
```

Output Markdown to a file:

```bash
review-engine review --local-path . --base main --format markdown --output review-report.md
```

---

## Review a GitLab MR or GitHub PR

### GitLab MR

```bash
review-engine review \
  --mr-url https://gitlab.com/owner/repo/-/merge_requests/42 \
  --gitlab-token glpat-xxx
```

Publish the report back to the MR discussion:

```bash
review-engine review \
  --mr-url https://gitlab.com/owner/repo/-/merge_requests/42 \
  --gitlab-token glpat-xxx \
  --publish
```

### GitHub PR

```bash
review-engine review \
  --mr-url https://github.com/owner/repo/pull/123 \
  --github-token ghp_xxx
```

Publish results back to the PR:

```bash
review-engine review \
  --mr-url https://github.com/owner/repo/pull/123 \
  --github-token ghp_xxx \
  --publish
```

> The token only needs read access to fetch the diff; add `--publish` only if the token also has permission to write discussions/comments.

---

## Where reports are saved

When you do **not** pass `--output`, review-engine prints the report to stdout and also saves a timestamped copy under the configured `output_dir`.

Default location:

```text
~/.config/review-engine/reports/review_YYYYMMDD_HHMMSS.<ext>
```

The extension matches the format: `.json` for JSON output or `.md` for Markdown output. You can change `output_dir` in `.code-audit-config.toml`.

---

## Next steps

- Read the full configuration guide: [`docs/configuration.md`](configuration.md)
- Set up webhooks or CI: [`docs/integrations/README.md`](integrations/README.md)
- Read the project overview: [`README.md`](../README.md)
