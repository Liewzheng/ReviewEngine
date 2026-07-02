# ReviewEngine

> A virtual **CodeReview Board** for every pull request — multi-expert, scored, and actionable.

[![License](https://img.shields.io/badge/License-Apache%202.0-blue.svg)](LICENSE)

**Free for individuals · Enterprise features available**

[中文文档](README.zh-CN.md)

ReviewEngine is released under the [Apache License 2.0](LICENSE). The core CLI, local review, GitLab/GitHub integrations, REST API, and default expert team are free and open source. Enterprise features such as SSO, audit logs, custom expert templates, and dedicated support are offered separately under a commercial license.

---

## Why ReviewEngine?

Consistent, deep code review is hard. Teams are busy, context is fragmented, and it's easy to miss security gaps, performance regressions, or reuse opportunities — especially in large diffs or unfamiliar code.

ReviewEngine brings a virtual engineering team to every review: a configurable **CodeReview Board** where multiple AI experts look at the same change in parallel, each through their own lens, and produce structured, scored findings you can act on.

- **Parallel expert review** — Security, Performance, Quality, Reuse, Docs, and more review together.
- **Structured output** — Every finding includes severity, confidence, evidence, impact, recommendation, and effort.
- **Scored, comparable results** — Per-expert scores plus a weighted overall score and a clear risk level.
- **Runs where you work** — GitLab MR, GitHub PR, local repo, CI/CD, or REST API.

| How code review often feels            | How ReviewEngine approaches it                                                |
| -------------------------------------- | ------------------------------------------------------------------------------ |
| "Did anyone check for SQL injection?"  | A Security Lead is always on the Board and reports findings explicitly.        |
| "This diff is huge, where do I start?" | Experts focus on their domain; findings are consolidated into a scored report. |
| "Why did we approve this?"             | Every review has a weighted score and documented evidence.                     |
| "Another tool to host and maintain."   | One static binary. `install.sh`, configure, run.                               |

---

## Who is it for?

You might like ReviewEngine if:

- **You want depth, not just surface-level comments.** Multiple experts mean security, performance, quality, and documentation concerns are all reviewed in one pass.
- **You want review _before_ opening a PR/MR.** Run it locally against `main`, staged changes, or a commit range and fix issues early.
- **You want risk signals, not just opinions.** Weighted scores and risk levels (Low → Critical) make it easier to decide what needs action now.
- **You want simple deployment.** A single static binary and a TOML config file are all you need to get started.

---

## What you get

|                                        |                                                                                                                |
| -------------------------------------- | -------------------------------------------------------------------------------------------------------------- |
| 🧑‍⚖️ **Multi-expert Board**              | Configure a team of AI experts with distinct roles, focus areas, principles, and weights.                      |
| 📊 **Structured scoring & risk level** | Individual expert scores, weighted overall score, and risk level: Low / Low-Medium / Medium / High / Critical. |
| 💻 **Local-first review**              | Review `--local-path`, `--base`, `--staged`, `--since`, `--until` — no remote MR/PR required.                  |
| ⚡ **Single static binary**            | Install with `install.sh` and run anywhere: CI, laptop, or server.                                             |

---

## Quick demo

```markdown
# CodeReview Board Report

**Overall Score:** 72/100  
**Risk Level:** Medium

---

## 🔴 Critical — Security Lead

**Title:** Unvalidated user input passed directly to SQL builder

- **Severity:** Critical
- **Confidence:** High
- **Effort:** Medium
- **Impact:** Potential SQL injection allowing unauthorized data access
- **Evidence:** `src/db.rs:42` — `query.push_str(&user_input)` without parameterization
- **Recommendation:** Use parameterized queries or a prepared statement builder. Add an integration test with sqlmap or equivalent.

---

## 🟠 High — Performance Lead

**Title:** Nested loop over unindexed relation in hot path

- **Severity:** High
- **Confidence:** Medium
- **Effort:** Low
- **Impact:** O(n²) behavior under load; latency spikes likely with >10k rows
- **Evidence:** `src/search.rs:88` — loop inside `load_user_records()`
- **Recommendation:** Add a database index on `user_id` and consider batch fetching.

---

## Expert Scores

| Expert               | Score  | Weight |
| -------------------- | ------ | ------ |
| Security Lead        | 45/100 | 25%    |
| Performance Lead     | 70/100 | 20%    |
| Quality Lead         | 85/100 | 20%    |
| Reuse Lead           | 80/100 | 15%    |
| Docs Lead            | 90/100 | 10%    |
| Maintainability Lead | 78/100 | 10%    |
```

---

## Quick start

Install the latest static binary:

```bash
curl -fsSL https://raw.githubusercontent.com/Liewzheng/Review-Engine/master/install.sh | bash
```

The installer requires `curl`, `jq`, and `sha256sum` (Linux) or `shasum` (macOS).

> **Security tip:** You can also download the script first, inspect it, and run it locally:
> ```bash
> curl -fsSL https://raw.githubusercontent.com/Liewzheng/Review-Engine/master/install.sh -o install.sh
> # inspect install.sh, then:
> bash install.sh
> ```

Configure an LLM provider. DeepSeek is accessed through the OpenAI-compatible API:

```bash
export LLM_CONFIG='[{"provider":"openai","model":"deepseek-chat","api_key":"sk-your-key","api_base":"https://api.deepseek.com/v1","max_tokens":4096,"temperature":0.3}]'
```

Or run `review-engine init` to generate a `.code-audit-config.toml` for your project.

Run your first local review:

```bash
review-engine review --local-path . --base main
```

For a detailed walkthrough, see [`docs/getting-started.md`](docs/getting-started.md).  
For full CLI options, environment variables, LLM providers, and config reference, see [`docs/configuration.md`](docs/configuration.md), [`docs/integrations/`](docs/integrations/), and [`docs/rest-api.md`](docs/rest-api.md).

### Common options

| Option | Description |
|---|---|
| `--local-path <path>` | Path to the repository to review. |
| `--base <ref>` | Base ref to compare against (e.g. `main`). |
| `--staged` | Review staged changes only. |
| `--since <ref>` / `--until <ref>` | Review a commit range. |
| `--format <json or markdown>` | Output format. |
| `--output <file>` | Write the report to a file. |
| `--publish` | Publish the review back to the MR/PR discussion. |

---

## Supported LLM providers

ReviewEngine supports multiple LLM providers out of the box:

- **OpenAI** (e.g., GPT-4o)
- **Anthropic** (e.g., Claude)
- **DeepSeek**
- **Any OpenAI-compatible provider**

Configure providers in `.code-audit-config.toml` or via the `LLM_CONFIG` environment variable:

```toml
[[llm]]
provider = "openai"
model = "gpt-4o"
api_key = "sk-your-key"
api_base = "https://api.openai.com/v1"
max_tokens = 4096
temperature = 0.3
```

> **Security tip**: replace `sk-your-key` with a real key at runtime via the `LLM_CONFIG` environment variable or a secrets manager. Do not commit credentials to version control.

See [`docs/configuration.md`](docs/configuration.md) for the full configuration reference.

---

## Integrations

ReviewEngine fits into existing workflows through multiple entry points:

- **GitLab MR** — review via CLI with `--mr-url` or through webhook comments (`/review`, `/improve`).
- **GitHub PR** — review via CLI with `--mr-url` or webhook.
- **Local repository** — review working tree, staged changes, or commit ranges without a remote.
- **CI/CD** — run as a step in GitLab CI, GitHub Actions, or any pipeline.
- **REST API** — start `review-engine serve` and trigger reviews over HTTP.

```bash
# GitLab MR review (set GITLAB_TOKEN in your environment)
review-engine review --mr-url https://gitlab.com/owner/repo/-/merge_requests/42

# GitHub PR review (set GITHUB_TOKEN in your environment)
review-engine review --mr-url https://github.com/owner/repo/pull/123

# Start the REST / webhook server
review-engine serve --port 8080
```

> **Security tip:** Pass tokens via the `GITLAB_TOKEN` and `GITHUB_TOKEN` environment variables instead of `--gitlab-token` / `--github-token` command-line flags to avoid leaking them in shell history or process lists.

More examples and setup guides are in [`docs/integrations/`](docs/integrations/).

---

## AI Skill

ReviewEngine can also be used as an [Agent Skills](https://github.com/cline/agent-skills)-compatible AI skill, so you can trigger reviews directly from supported agents.

Supported agents include **Kimi Code**, **Claude Code**, **Codex CLI**, **OpenCode**, **Cursor**, and other Agent Skills-compatible clients.

Install the skill globally:

```bash
cp -R .kimi-code/skills/review-engine ~/.kimi-code/skills/
# or for Claude Code: ~/.claude/skills/
```

Once installed, trigger it with phrases like **"review-engine"**, **"review this repo"**, **"repo review"**, or **"review a PR"**.

For details, see [`.kimi-code/skills/review-engine/SKILL.md`](.kimi-code/skills/review-engine/SKILL.md).

---

## Architecture

```
Input → Config Resolution → Expert Selection → Parallel Review → Consolidation → Scored Report
```

- Built in **Rust** for fast startup and reliable concurrency.
- Distributed as a **single static binary** via `install.sh`.
- Config-driven expert team defined in `.code-audit-config.toml`.
- Parallel LLM calls with per-expert prompts, weights, and focus areas.
- Optional **REST API** (`review-engine serve`) for webhooks and frontends.
- Optional **repo-wide health check** (`review-engine repo-review`) for broader codebase analysis.

---

## Performance

ReviewEngine is designed to be lightweight and CI-friendly. Resource usage is dominated by LLM network latency, not local CPU or memory.

Benchmarked on a ~30k LOC repository (3 runs, `repo-review`, local CLI, DeepSeek model):

| Metric | Average |
|---|---|
| Wall time | ~5 m 46 s |
| Peak memory | ~9 MB |
| Max RSS | ~19 MB |
| CPU time | ~0.07 s |

For a typical branch/MR review, the `review` command usually completes in **30–50 s**, depending on the LLM provider and network conditions.

---

## Commands

ReviewEngine is organized around a small set of focused commands:

| Command            | Purpose                                                    |
| ------------------ | ---------------------------------------------------------- |
| `review`           | Run a CodeReview Board review on an MR, PR, or local diff. |
| `describe`         | Generate a summary or MR/PR description from a diff.       |
| `improve`          | Suggest concrete code improvements for a diff.             |
| `repo-review`      | Run a repo-wide health check across the entire codebase.   |
| `update_changelog` | Generate or update a changelog from recent commits.        |
| `serve`            | Start the REST API and webhook server.                     |
| `validate`         | Validate your `.code-audit-config.toml`.                   |
| `init`             | Generate a starter config for a new project.               |
| `default`          | Print the built-in default config.                         |
| `generate-token`   | Generate a random API token for `review-engine serve`.     |

---

## Roadmap & Community

- See [`CHANGELOG.md`](CHANGELOG.md) for recent releases and upcoming milestones.
- See [`CONTRIBUTING.md`](CONTRIBUTING.md) for how to contribute code, docs, or issues.

We welcome contributors. Whether it's a bug report, a docs improvement, or a new expert idea, open an issue or pull request and we'll take a look.

---

## Enterprise

- **Core** — Apache-2.0, free for individuals and teams, developed in this repository.
- **Enterprise** — SSO, audit logs, custom expert templates, advanced analytics, and dedicated support are offered separately.

For details on enterprise offerings, see [`docs/enterprise.md`](docs/enterprise.md).

---

## License

The ReviewEngine core is licensed under the [Apache License 2.0](LICENSE). Enterprise features and commercial support are developed separately and are not part of this open-source repository.
