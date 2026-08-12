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
- It posts inline comments on specific files and lines for **Critical** and **High** severity findings.
- The dispatcher tracks the latest commit SHA to avoid duplicate reviews.

## Next steps

- See the [GitLab webhook setup](gitlab.md) for a similar configuration on GitLab.
- Add review-engine to your CI pipeline: [CI pipeline examples](ci-examples.md).
