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
- It posts inline comments on files and lines, chosen by the **inline-note delivery policy** (below). The candidate set is the **consolidated** finding set — deduplicated across experts and filtered by the adjudication pass — and a comment is only posted when its line is part of the reviewed diff. Each comment opens with its `` `path:line` `` anchor. Unlike GitLab, GitHub's comment API addresses a line by one side (`line` + `side=RIGHT`), so the old-side number an anchor may carry is ignored here (RENG-99).
- A single failing comment no longer stops the batch: permanent rejections are logged and skipped, transient provider errors are retried up to three attempts, and the run logs a `posted / rolled up / policy-excluded / anchor-ineligible / skipped / failed` summary. **The HTTP status decides** whether a failure is transient (RENG-65): 408/429/5xx are retried, every other status is permanent; only a failure with no HTTP answer at all — a transport failure — falls back to the message. A `404` whose response body mentions `connection closed` is therefore *not* retried.
- **A refused comment is visible in the report, not only in the log** (RENG-99): the board is updated with an `## Inline notes — N could not be published` section naming each refused `` `path:line` ``, its HTTP status and GitHub's verdict, and the publish failure is recorded on the review result so `GET /api/v1/reviews/{id}` reports it too.
- **A rejected comment logs its cause** (RENG-71): one `WARN` per failure, naming the finding's `file:line`, the line that was submitted, the HTTP status and GitHub's response body — truncated to 512 characters by the same helper the LLM error samples use — so a rejected position is distinguishable from a `403`, a `404` or a transport failure without reproducing it.
- The dispatcher tracks the latest commit SHA to avoid duplicate reviews.

### Inline-note delivery policy

Not every high-severity finding deserves its own line-anchored comment. A round's findings pass through `PublishPolicy` (`src/publisher/policy.rs`), whose defaults are: **severity `high` or above**, **confidence `8/10` or above**, **a non-empty (actionable) recommendation**, and **an anchor inside the reviewed diff** — plus three delivery rules:

- **At most 2 inline comments per round.** The rest of the admitted findings are rolled up into the board. When the cap binds, the highest-ranked findings are the ones posted; the ranking is deterministic (severity descending, then confidence descending, then file path, line, title and expert name ascending).
- **A documentation- or CI-only change is published summary-only** — the `# CodeReview Board` comment, zero inline comments. A change set counts as docs/CI-only when *every* changed file is documentation or CI config: `.md`/`.mdx`/`.rst`/`.adoc`/`.asciidoc`, anything under `docs/`, `doc/`, `documentation/`, `man/`, license/notice files, anything under `.github/`, `.gitlab/`, `.circleci/`, `.buildkite/`, `.woodpecker/`, `.travis/`, `.ci/`, `ci/`, and CI files such as `.gitlab-ci.yml`, `Jenkinsfile`, `codecov.yml`.
- **Everything the policy withholds stays on the board.** The comment gains an `## Inline notes — delivery policy` section naming the findings it withheld (severity, `` `path:line` ``, title, confidence) and the rule that withheld them; the per-expert sections are unaffected.

The policy is built in code with those defaults and can be overridden with four environment variables — see [Inline-note delivery policy](../configuration.md#inline-note-delivery-policy).

### Adjudication on server-side reviews

The final false-positive filter — the *adjudication pass* (`[report] adjudicate`, on by default at `adjudicate_min_severity = "high"`) — re-reads each high-severity finding with the lead model **against the full current content of the cited file** and drops claims the code disproves. A webhook review never clones the repository, so since 0.10.22 the cited file is fetched through the GitHub API at the reviewed commit SHA (`GET /repos/:owner/:repo/contents/:path?ref=<sha>` with the raw media type, using the same token as the diff fetch) instead of from a checkout. Local reviews (`--local-path`) keep reading their working tree.

- **Full file, not the patch.** The diff a review is triggered on carries only the changed regions ±3 context lines, so it is never used as ground truth here.
- **Fail-open, with the reason logged.** A file the revision does not contain (404), a transport or decode failure, or a token that may not read the repository (401/403 — GitHub contents read) keeps every finding of that file unchanged, and says so: one `WARN` per file for a missing/failed file, one for the whole pass for a credential problem. If no source at all is available (no token, or the reviewed SHA could not be resolved) the pass warns and keeps every candidate.
- **The log line states the source.** Each pass ends with `Adjudication: source=<local|provider-api> files=N fetches=N dropped=N kept=N`. Files are fetched once per `(path, revision)` for the pass, at most 4 at a time and under a bounded budget.

See [`[report]`](../config-schema.md#report) for the knobs.

## Next steps

- See the [GitLab webhook setup](gitlab.md) for a similar configuration on GitLab.
- Add review-engine to your CI pipeline: [CI pipeline examples](ci-examples.md).
