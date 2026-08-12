---
name: review-engine
description: >
  MUST EXECUTE when the user says "review-engine", "run review-engine",
  "review this repo", "repo review", "review a PR", "review an MR",
  "review a GitHub PR", "review a GitLab MR", or "audit this repository".
  Runs the ReviewEngine multi-expert AI review board on a local repository,
  GitHub PR, or GitLab MR to produce scored, structured reports with
  severity, confidence, evidence, and actionable recommendations. Can also
  publish findings back to the PR/MR discussion.
license: Apache-2.0
compatibility:
  - kimi-code
  - agent-skills
metadata:
  author: ReviewEngine contributors
  version: 0.6.10
---

# ReviewEngine Skill

Run [ReviewEngine](https://github.com/Liewzheng/Review-Engine) to review a
repository, GitHub PR, or GitLab MR with a configurable board of AI experts.
Each expert reviews from its own lens (security, performance, quality, reuse,
docs, and more) and produces structured, scored findings.

> **Do not use this skill for generic "code review" or "code audit" requests.**
> Those are handled by the `code-audit` user skill. Only trigger this skill for
> the ReviewEngine-specific phrases listed in the description.

## Install

Latest static binary via GitHub releases:

```bash
curl -fsSL https://raw.githubusercontent.com/Liewzheng/Review-Engine/master/install.sh | bash
```

Build from source:

```bash
cargo install --git https://github.com/Liewzheng/Review-Engine.git review-engine
```

The installer copies the binary to `~/.local/bin` and installs a default user
config at `~/.config/review-engine/.code-audit-config.toml`.

## Configure

All commands are disabled by default. Enable the ones you need and add at least
one LLM provider.

Quick start with an environment variable:

```bash
export LLM_CONFIG='[{"provider":"openai","model":"deepseek-chat","api_key":"sk-your-key","api_base":"https://api.deepseek.com/v1","max_tokens":4096,"temperature":0.3}]'
```

Or generate a project config interactively:

```bash
reng init
```

For a complete config reference see [references/config.md](references/config.md).

## Common commands

| Task | Command |
|------|---------|
| Audit the whole repository | `reng audit --local-path .` |
| Review current branch vs `main` | `reng review --local-path . --base main` |
| 全量审查仓库内单个子目录/子模块 | `reng review --local-path . --path <dir>` |
| Review a GitHub PR / GitLab MR | `reng review --mr-url <URL> --publish` |
| Generate a PR/MR description | `reng describe --mr-url <URL>` |
| Suggest concrete improvements | `reng improve --mr-url <URL>` |
| Validate the config file | `reng validate --config .code-audit-config.toml` |
| Start the REST / webhook server | `reng serve --port 8080` |
| Check for / apply a self-upgrade | `reng upgrade` (report only: `reng upgrade --check`) |
| Roll back a bad upgrade | `reng upgrade --rollback` |

`reng` is the short alias for `review-engine` — both names run the same binary.
`audit` is the alias for `repo-review` (whole-repo health checks); branch and
MR reviews stay under `review`.

See [references/commands.md](references/commands.md) for the full command list
and more examples.

## Output formats and publishing

- `--format markdown` (default for `audit`) prints a human-readable report.
- `--format json` (default for `review`) emits structured data for CI pipelines.
- `--output report.md` writes the report to a file; otherwise a timestamped copy
  is saved under `~/.config/review-engine/reports/`.
- `--publish` posts the report back to the GitHub PR or GitLab MR discussion.
  The token needs write permission for discussions/comments.

## Security reminders

- Pass API keys through the `LLM_CONFIG` environment variable or a secrets
  manager. Do not commit them to version control.
- Pass `GITLAB_TOKEN` and `GITHUB_TOKEN` as environment variables instead of
  `--gitlab-token` / `--github-token` flags to avoid leaking them in shell
  history or process lists.
