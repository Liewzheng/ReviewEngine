# GitLab Webhook Integration

This guide shows how to run review-engine as a webhook server that automatically reviews GitLab merge requests and responds to comment commands.

## Prerequisites

- A GitLab project where you have Maintainer or Owner access.
- A running review-engine binary.
- A GitLab personal or project access token with `api` and `read_repository` scopes.
- A webhook secret token of your choice.

## Start the server

The GitLab webhook handler reads the API token from the `GITLAB_TOKEN` environment variable.

For webhook authentication you can use either the legacy **secret token** or the new **signing token** (GitLab 19.0+). You can also configure both during a migration.

### Option A — legacy secret token

The secret token is sent in plain text in the `X-Gitlab-Token` header.

```bash
export GITLAB_TOKEN="glpat-xxx"
export GITLAB_WEBHOOK_SECRET="a-strong-random-secret"

review-engine serve --port 8080
```

### Option B — signing token (recommended, GitLab 19.0+)

The signing token uses HMAC-SHA256 and follows the Standard Webhooks specification. Copy the entire value shown by GitLab, including the `whsec_` prefix.

```bash
export GITLAB_TOKEN="glpat-xxx"
export GITLAB_WEBHOOK_SIGNING_SECRET="whsec_..."

review-engine serve --port 8080
```

Make sure the server is reachable from GitLab. For testing you can use a local tunnel such as `ngrok`:

```bash
ngrok http 8080
```

## Configure the webhook in GitLab

1. Go to **Settings → Webhooks** in your GitLab project.
2. **URL**: `https://your-server.example.com/webhook/gitlab`
3. Authentication:
   - For the legacy secret token, enter the same value you set as `GITLAB_WEBHOOK_SECRET` in the **Secret token** field.
   - For GitLab 19.0+, select **Generate signing token**, copy the value, and set it as `GITLAB_WEBHOOK_SIGNING_SECRET`.
4. **Trigger events**:
   - **Merge request events** — required for automatic review on open/reopen/update.
   - **Comments** — required for `/review`, `/improve`, `/describe` commands.
5. Click **Add webhook** and test with a merge request event if desired.

## Comment commands

Team members can trigger actions by commenting on an MR:

| Command | Action |
|---|---|
| `/review` | Run a full CodeReview Board review. |
| `/improve` | Generate concrete code improvement suggestions. |
| `/describe` | Generate or update an MR description from the diff. |

## How it posts back

When a review finishes, review-engine publishes the results back to the MR discussion:

- It creates (or updates) a top-level discussion note titled `# CodeReview Board`.
- It posts inline notes on specific files and lines for **Critical** and **High** severity findings.
- The dispatcher tracks the latest commit SHA to avoid running duplicate reviews for the same SHA.

## Next steps

- See the [GitHub webhook setup](github.md) for a similar configuration on GitHub.
- Add review-engine to your CI pipeline: [CI pipeline examples](ci-examples.md).
