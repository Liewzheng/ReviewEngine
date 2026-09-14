# GitHub Webhook Integration

This guide shows how to run review-engine as a webhook server that automatically reviews GitHub pull requests and responds to comment commands.

## Prerequisites

- A GitHub repository where you have admin or owner access.
- A running review-engine binary.
- A GitHub personal access token with `repo` scope.
- A webhook secret token of your choice.

## Start the server

Start `review-engine serve` with your GitHub token and webhook secret:

```bash
export GITHUB_TOKEN="ghp_xxx"
export GITHUB_WEBHOOK_SECRET="a-strong-random-secret"

review-engine serve --port 8080 \
  --github-token "$GITHUB_TOKEN" \
  --github-webhook-secret "$GITHUB_WEBHOOK_SECRET"
```

You can also pass the values as environment variables and omit the flags:

```bash
export GITHUB_TOKEN="ghp_xxx"
export GITHUB_WEBHOOK_SECRET="a-strong-random-secret"

review-engine serve --port 8080
```

For testing locally, expose the server with a tunnel such as `ngrok`:

```bash
ngrok http 8080
```

## Configure the webhook in GitHub

1. Go to **Settings → Webhooks → Add webhook** in your repository.
2. **Payload URL**: `https://your-server.example.com/webhook/github`
3. **Content type**: `application/json`
4. **Secret**: the same value you set as `GITHUB_WEBHOOK_SECRET`.
5. **Events**:
   - **Pull requests** — required for automatic review on opened/reopened/synchronized.
   - **Pull request review comments** — enables comment-triggered reviews.
   - **Issue comments** — enables `/review`, `/improve`, `/describe` commands on PRs.
6. Click **Add webhook**.

## Comment commands

Anyone with access to comment on the PR can trigger review-engine:

| Command | Action |
|---|---|
| `/review` | Run a full CodeReview Board review. |
| `/improve` | Generate concrete code improvement suggestions. |
| `/describe` | Generate or update a PR description from the diff. |

## How it posts back

When a review finishes, review-engine publishes the results back to the PR:

- It creates (or updates) a top-level review discussion titled `# CodeReview Board`.
- It posts inline comments on files and lines, chosen by the **inline-note delivery policy** (below). The candidate set is the **consolidated** finding set — deduplicated across experts and filtered by the adjudication pass — and a comment is only posted when its line is part of the reviewed diff. Each comment opens with its `` `path:line` `` anchor.
- A single failing comment no longer stops the batch: permanent rejections are logged and skipped, transient provider errors (transport failure, 408/429/5xx) are retried up to three attempts, and the run logs a `posted / rolled up / policy-excluded / anchor-ineligible / skipped / failed` summary.
- The dispatcher tracks the latest commit SHA to avoid duplicate reviews.

### Inline-note delivery policy

Not every high-severity finding deserves its own line-anchored comment. A round's findings pass through `PublishPolicy` (`src/publisher/policy.rs`), whose defaults are: **severity `high` or above**, **confidence `8/10` or above**, **a non-empty (actionable) recommendation**, and **an anchor inside the reviewed diff** — plus three delivery rules:

- **At most 2 inline comments per round.** The rest of the admitted findings are rolled up into the board. When the cap binds, the highest-ranked findings are the ones posted; the ranking is deterministic (severity descending, then confidence descending, then file path, line, title and expert name ascending).
- **A documentation- or CI-only change is published summary-only** — the `# CodeReview Board` comment, zero inline comments. A change set counts as docs/CI-only when *every* changed file is documentation or CI config: `.md`/`.mdx`/`.rst`/`.adoc`/`.asciidoc`, anything under `docs/`, `doc/`, `documentation/`, `man/`, license/notice files, anything under `.github/`, `.gitlab/`, `.circleci/`, `.buildkite/`, `.woodpecker/`, `.travis/`, `.ci/`, `ci/`, and CI files such as `.gitlab-ci.yml`, `Jenkinsfile`, `codecov.yml`.
- **Everything the policy withholds stays on the board.** The comment gains an `## Inline notes — delivery policy` section naming the findings it withheld (severity, `` `path:line` ``, title, confidence) and the rule that withheld them; the per-expert sections are unaffected.

The policy is built in code with those defaults and can be overridden with four environment variables — see [Inline-note delivery policy](../configuration.md#inline-note-delivery-policy).

## Next steps

- See the [GitLab webhook setup](gitlab.md) for a similar configuration on GitLab.
- Add review-engine to your CI pipeline: [CI pipeline examples](ci-examples.md).
