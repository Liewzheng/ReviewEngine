# GitLab Webhook Integration

This guide shows how to run review-engine as a webhook server that automatically reviews GitLab merge requests and responds to comment commands. It works with **project-level webhooks** and with a single **admin-level System Hook** that covers every project on the instance.

## Prerequisites

- A GitLab project where you have Maintainer or Owner access — or **Admin** access to configure a System Hook.
- A running review-engine binary.
- A GitLab personal or project access token with `api` and `read_repository` scopes.
- A webhook secret token of your choice.

## Start the server

GitLab credentials and webhook verification secrets are configured per Git platform in the **Web UI** (Git 平台 card, `/#/config`) and are **hot-effective** — no restart, no environment variables. They are persisted to `ui-state.toml` and replayed at startup.

`GITLAB_TOKEN`, `GITLAB_WEBHOOK_SECRET`, `GITLAB_WEBHOOK_SIGNING_SECRET` (and their `--gitlab-*` flags) are **fallback-only and deprecated**: they take effect only when the persisted UI state holds no value for that field, and each such use logs a deprecation warning. Configure the credentials in the Web UI instead.

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

You can also save the signing token through the **Configuration UI** (`/#/config`). The value is persisted via `PUT /api/v1/config` into the Git 平台 entry for the instance (the recommended path — it survives restarts and is hot-effective). It is used as a fallback when neither `--gitlab-webhook-signing-secret` nor `GITLAB_WEBHOOK_SIGNING_SECRET` is set.

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

### Option 1 — project-level webhook (per project)

1. Go to **Settings → Webhooks** in your GitLab project.
2. **URL**: `https://your-server.example.com/webhook/gitlab`
3. Authentication:
   - For the legacy secret token, enter the same value you configured as the platform's **Secret token** (`webhook_secret`) in the **Secret token** field.
   - For GitLab 19.0+, select **Generate signing token**, copy the value, and set it as the platform's **Signing token** (`webhook_signing_secret`).
4. **Trigger events**:
   - **Merge request events** — required for automatic review on open/reopen/update.
   - **Comments** — required for `/review`, `/improve`, `/describe` commands.
5. Click **Add webhook** and test with a merge request event if desired.

### Option 2 — admin-level System Hook (all projects, recommended for many projects)

A single System Hook covers **every project** on the instance, so you no longer need to register a webhook per project. GitLab sends it with the header `X-Gitlab-Event: System Hook`; review-engine routes it by the event type in the payload body (see below).

1. Go to **Admin Area → System Hooks** (requires GitLab **Admin** access).
2. **URL**: `https://your-server.example.com/webhook/gitlab` (same endpoint as project webhooks).
3. Authentication — identical to project webhooks: either the **Secret token** (`X-Gitlab-Token` header) or the **Signing token** (GitLab 19.0+, Standard Webhooks HMAC-SHA256). Configure the matching value in the Git 平台 entry for the instance, exactly as for a project webhook.
4. **Trigger**:
   - **Merge request events** — automatic review on open/reopen/update.
   - **Comment events** — `/review`, `/improve`, `/describe` commands.
   - **Push events** — accepted (acknowledged and logged).
5. Click **Add system hook**. GitLab can only send events for projects that exist on the instance; combine with the project allowlist below to restrict which projects actually trigger reviews.

### System Hook payload handling

- **Event routing**: the event type is read from the payload's `event_name` (older GitLab) or `event_type` (GitLab 19+). Both are accepted, with `event_name` preferred. `merge_request`, `note`, and `push` map to the same handlers as the corresponding project hooks; any other event type is ignored.
- **No `project.web_url`**: System Hook payloads omit `project.web_url`. To match the payload to a configured Git platform, review-engine falls back through `project.homepage` → `repository.homepage` → `object_attributes.url`. Merge-request events read the full MR URL from `object_attributes.url`; note events rebuild it from `project.web_url`/`project.homepage` plus the MR iid (extracted from the URL tail when the payload omits the iid).
- **Verification** is the same as for project webhooks: the matched platform's Secret token / Signing token is used to verify `X-Gitlab-Token` / `webhook-signature`.

## Project filtering (allowed projects)

A Git 平台 entry can carry an **allowed projects** allowlist (`allowedProjects` in the Web UI, `allowed_projects` in config) of `path_with_namespace` values (e.g. `group/project`). This is most useful with System Hooks, which otherwise cover every project:

- **Empty (default) = every project is allowed.**
- **Non-empty**: only projects whose `path_with_namespace` is listed can trigger a review. An event from an unlisted project is answered with `200 {"status":"ignored","reason":"project not in allowlist"}` — acknowledged, not reviewed.
- The allowlist is **hot-effective**: saving it in the Web UI applies immediately, no restart.
- Matching is exact and case-sensitive; the list only gates webhook-triggered reviews (REST `gitlab_mr` reviews are unaffected).

## Internal URL (dual-network)

A Git 平台 entry has two URL fields for setups where the payload's `external_url` is not reachable from inside the review container (typical on a NAS behind a port mapping, or a dev box where GitLab binds `localhost`):

- **Base URL** (`baseUrl` / `base_url`) — the external address that appears in GitLab payloads. Used to **match** which platform a webhook belongs to.
- **Internal URL** (`internalBaseUrl` / `internal_base_url`, optional) — the address review-engine actually **reaches at review time** to pull GitLab API data (e.g. `http://host.docker.internal:8929`, or `https://gitlab.islet.space` on the container-internal 443 when the external `external_url` is `https://gitlab.islet.space:8443`).

When set, review-time MR URLs are rewritten onto the internal URL before any GitLab API call. Leave it empty to fall back to **Base URL**, then to the payload URL if that also cannot be parsed. Changing the internal URL does not change webhook matching (only `base_url` identifies the instance).

### Manual submissions (REST `POST /api/v1/reviews`)

The same rewrite applies to a manually submitted `gitlab_mr` URL (RENG-33), using the same matcher and the same rewrite function the webhook path uses:

- A submitted URL whose `host[:port]` matches an entry's **Base URL** — or its **Internal URL**, when configured — is re-hosted onto that entry's **Internal URL** (falling back to **Base URL**) before the review runs. Path and query string are preserved; the host is compared case-insensitively, an explicitly written default port (80/443) folds to "unwritten", any other port is compared strictly, and the path / trailing slash never take part in the match.
- The rewritten URL is what the review fetches **and** what the task records as its MR URL (`gitlabMrUrl`); the address you typed is not stored (it is logged once at submit time with the platform it matched).
- A submitted URL whose host is a local address (`localhost`, `127.0.0.0/8`, `0.0.0.0`, `::1`) that **no** entry matches is rejected up front with `400` and an actionable message — submit the address the server can reach (`host.docker.internal` inside Docker) instead, or add/edit an entry so `baseUrl` covers the address you browse to and `internalBaseUrl` the address the server reaches. Nothing is queued, so no empty "Untitled Review" row appears in the history.
- A host no entry claims and that is not a local address is fetched exactly as submitted (e.g. `https://gitlab.com/...`).

See `docs/rest-api.md` §1 for the exact status codes and error text.

## Security details

When signing is enabled, GitLab sends the following Standard Webhooks headers in every request:

- `webhook-id` — a unique UUID for the event.
- `webhook-timestamp` — the Unix timestamp when the event was signed.
- `webhook-signature` — one or more HMAC-SHA256 signatures (`v1,...`) computed from the request body.

review-engine:

1. Verifies at least one signature in `webhook-signature` matches the secret.
2. Validates the timestamp to prevent replay attacks (the default tolerance is 5 minutes).
3. Rejects requests with a stale timestamp or an invalid signature.

Ensure the server clock is synchronized with NTP. If you see `webhook timestamp too old` errors, the system time on the review-engine host is likely out of sync with GitLab.

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
- It posts inline notes on specific files and lines for **Critical** and **High** severity findings. The inline set is the **consolidated** finding set — deduplicated across experts and filtered by the adjudication pass — and a note is only posted when its line is part of the reviewed diff, so rejected anchors (`line_code can't be blank`) no longer occur. Each note opens with its `` `path:line` `` anchor.
- A single failing note no longer stops the batch: permanent rejections are logged and skipped, transient provider errors (transport failure, 408/429/5xx) are retried up to three attempts, and the run logs a `posted / skipped / failed` summary.
- The dispatcher tracks the latest commit SHA to avoid running duplicate reviews for the same SHA.

## Next steps

- See the [GitHub webhook setup](github.md) for a similar configuration on GitHub.
- Add review-engine to your CI pipeline: [CI pipeline examples](ci-examples.md).
