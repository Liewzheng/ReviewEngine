# Integrations

review-engine fits into existing Git workflows through webhooks, CI pipelines, and the REST API.

## Guides

- [GitLab webhook setup](gitlab.md) — trigger reviews from MR events and `/review`, `/improve`, `/describe` comments.
- [GitHub webhook setup](github.md) — trigger reviews from PR events and comment commands.
- [CI pipeline examples](ci-examples.md) — run review-engine in GitLab CI or GitHub Actions.

## Quick reference

```bash
# GitLab MR from the CLI
review-engine review \
  --mr-url https://gitlab.com/owner/repo/-/merge_requests/42 \
  --gitlab-token glpat-xxx

# GitHub PR from the CLI
review-engine review \
  --mr-url https://github.com/owner/repo/pull/123 \
  --github-token ghp_xxx

# Local review
review-engine review --local-path . --base main

# Start the REST / webhook server
review-engine serve --port 8080
```

For the full REST API reference, see [`docs/rest-api.md`](../rest-api.md).
