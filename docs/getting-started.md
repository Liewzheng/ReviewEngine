# Getting Started with review-engine

This guide walks you through installing review-engine and running your first review.

---

## Install review-engine

The easiest way to install review-engine is with the `install.sh` script. It downloads a single static binary and places it in `~/.local/bin`.

### Stable release (recommended)

```bash
curl -fsSL https://raw.githubusercontent.com/Liewzheng/ReviewEngine/master/install.sh | bash
```

The script detects your platform, resolves the latest stable release, verifies the SHA256 checksum, and copies the default config to `~/.config/review-engine/.code-audit-config.toml`.

The installer also creates a `reng` symlink next to the binary. `reng` and `review-engine` are the same command — examples below use `reng`.

### Source build

If you prefer to build from source, or a binary is not available for your platform:

```bash
curl -fsSL https://raw.githubusercontent.com/Liewzheng/ReviewEngine/master/install.sh | bash -s -- --source
```

This requires `git` and `cargo`.

### Daily / pre-release builds

To install a specific version (for example a daily or pre-release tag), set `REVIEW_ENGINE_VERSION`:

```bash
export REVIEW_ENGINE_VERSION="v0.x.x"
curl -fsSL https://raw.githubusercontent.com/Liewzheng/ReviewEngine/master/install.sh | bash
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
reng review --local-path . --base main
```

Review only staged changes:

```bash
reng review --local-path . --staged
```

Review a commit range:

```bash
reng review --local-path . --since HEAD~3 --until HEAD
```

Output Markdown to a file:

```bash
reng review --local-path . --base main --format markdown --output review-report.md
```

Run a whole-repository health audit (`audit` is the alias of `repo-review`):

```bash
reng audit --local-path . --format markdown
```

---

## Review a single subdirectory (子库/子目录单独审查)

`review --path` runs a review on one subdirectory or submodule of a repository. Use it when a large monorepo keeps a subproject outside normal PR-based reviews.

### Core command

```bash
reng review --local-path <repo> --path <dir>
```

`--path` triggers a **full-content review**: every reviewable file in the directory is treated as newly added code against a synthetic empty-tree diff (`--- /dev/null`, with the whole file as `+` lines). The normal expert pipeline applies — large-PR chunking, full-file content injection, and finding validation — so a big directory is split and covered exactly like a large PR.

### Examples

Review `src/actions` in the current checkout and write a Markdown report:

```bash
reng review --local-path . --path src/actions --format markdown --output report.md --progress
```

Review a submodule inside a repository checked out elsewhere:

```bash
reng review --local-path /path/to/repo --path packages/parser --format json --output parser-review.json
```

> Only want the *recent changes* in a directory? Build a diff for it and review that instead:
>
> ```bash
> git diff <base> -- <dir> > changes.diff
> reng review --diff changes.diff
> ```

### Constraints

- `--path` must be a **relative path** to the repository root; absolute paths and `..` are rejected.
- `--path` must be combined with `--local-path`, which names the repository root.
- `--path` is mutually exclusive with `--mr-url`, `--diff`, `--stdin`, `--base`, `--head`, `--since`, `--until`, and `--staged`.
- If the directory does not exist, is empty, or contains no reviewable files, review-engine exits with an error instead of producing an empty report.

### What gets reviewed and what is skipped

Under a git repository, "reviewable" means the files reported by `git ls-files --cached --others --exclude-standard` — tracked files plus untracked files that are not gitignored. Outside git, the directory is walked recursively while skipping the fixed ignore list.

Files that cannot be reviewed are skipped silently rather than flagged: symlinks, non-UTF-8 files, and filtered extensions such as `.lock`, `.sum`, `.png`, `.min.js`, and `package-lock.json`. Dependency and build output directories — `node_modules/`, `target/`, `dist/`, `build/`, `.venv/`, `vendor/` — are also ignored, so `frontend/dist` is skipped automatically.

### Zero-findings reports

A full-content review reads every file, so a report with zero findings still appends a coverage statement: it records how many files were covered in full and notes that zero findings does not mean the code is problem-free.

### How this differs from `audit` and `--base`

- `reng audit` runs whole-repository health checks, not a per-file diff-style review.
- `reng review --base` reviews the diff between branches or commits.
- `reng review --path` reviews the full current content of one directory inside the repository.

---

## Review a GitLab MR or GitHub PR

### GitLab MR

```bash
reng review \
  --mr-url https://gitlab.com/owner/repo/-/merge_requests/42 \
  --gitlab-token glpat-xxx
```

Publish the report back to the MR discussion:

```bash
reng review \
  --mr-url https://gitlab.com/owner/repo/-/merge_requests/42 \
  --gitlab-token glpat-xxx \
  --publish
```

### GitHub PR

```bash
reng review \
  --mr-url https://github.com/owner/repo/pull/123 \
  --github-token ghp_xxx
```

Publish results back to the PR:

```bash
reng review \
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
