# ReviewEngine Command Reference

## Subcommand overview

| Subcommand | Purpose |
|------------|---------|
| `review` | Run a multi-expert review on a local diff, GitHub PR, or GitLab MR. |
| `audit` | Run a repo-wide health check across the entire codebase (alias of `repo-review`). |
| `describe` | Generate a summary or PR/MR description from a diff. |
| `improve` | Suggest concrete code improvements for a diff. |
| `ask` | Ask a question about the diff (requires command enablement). |
| `update_changelog` | Generate or update a changelog from recent commits. |
| `serve` | Start the REST API and webhook server. |
| `upgrade` | Check for and apply self-upgrades (also `--check`, `--version`, `--rollback`). |
| `validate` | Validate a `.code-audit-config.toml` file. |
| `init` | Generate a starter config for the current project. |
| `default` | Print the built-in default config. |
| `generate-token` | Generate a random API token for `reng serve`. |

`reng` is the short alias for `review-engine` (same binary); `audit` is the
alias for `repo-review`. Examples below use `reng`.

## Usage examples

### Repo-wide audit

```bash
reng audit --local-path .
```

### Local branch review

```bash
reng review --local-path . --base main
```

Write Markdown to a file:

```bash
reng review \
  --local-path . \
  --base main \
  --format markdown \
  --output report.md
```

### Review a GitHub PR or GitLab MR

```bash
# GitHub PR
reng review \
  --mr-url https://github.com/owner/repo/pull/123 \
  --github-token ghp_xxx

# GitLab MR
reng review \
  --mr-url https://gitlab.com/owner/repo/-/merge_requests/42 \
  --gitlab-token glpat-xxx
```

Publish the report back to the PR/MR discussion:

```bash
reng review \
  --mr-url https://github.com/owner/repo/pull/123 \
  --github-token ghp_xxx \
  --publish
```

### Describe a PR/MR

```bash
reng describe --mr-url https://github.com/owner/repo/pull/123
```

### Improve a PR/MR

```bash
reng improve --mr-url https://gitlab.com/owner/repo/-/merge_requests/42
```

### Validate configuration

```bash
reng validate --config .code-audit-config.toml
```

### Start the REST / webhook server

```bash
reng serve --port 8080
```

Optional generated token for server authentication:

```bash
reng generate-token
```

### Generate a starter config

```bash
reng init
```

Print the built-in default config without prompts:

```bash
reng init --default
```

### Check for and apply self-upgrades

```bash
reng upgrade              # check + confirm + apply (plain binary installs)
reng upgrade --check      # report the latest version only
reng upgrade --version v0.9.0  # target a specific release (latest only)
reng upgrade --rollback   # restore the previous binary from review-engine.bak
```

### Update changelog

```bash
reng update_changelog --local-path .
```
