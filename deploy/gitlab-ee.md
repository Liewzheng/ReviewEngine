# Deploy Review-Engine for GitLab EE (Self-Hosted)

This guide walks you through deploying **review-engine** as a Docker container for GitLab Enterprise Edition (self-hosted) code review automation.

---

## 📋 Prerequisites

- Docker Engine ≥ 24.0 + Docker Compose v2
- GitLab EE instance with admin or project owner access
- LLM API keys (OpenAI, Anthropic, or compatible)
- A server with ≥ 2 CPU cores, 2 GB RAM, 10 GB disk

---

## 🚀 Quick Start (5 minutes)

### 1. Clone and Configure

```bash
git clone https://github.com/Liewzheng/ReviewEngine.git
cd ReviewEngine

# If you forked the repo or deploy from a private mirror, replace the URL
# above with your own repository URL.

# Copy environment template
cp .env.example .env

# Edit .env with your credentials
nano .env
```

### 2. Required Environment Variables

Edit `.env` and set these **required** variables:

```bash
# docker compose does NOT expand $(...) inside .env — it reads the file
# literally, so `REVIEW_API_TOKEN=$(openssl rand -hex 32)` would store the
# literal string "$(openssl rand -hex 32)" as the value.
# Generate each value in your shell FIRST, then paste the output into .env:
openssl rand -hex 32                            # -> REVIEW_API_TOKEN
openssl rand -hex 32                            # -> GITLAB_WEBHOOK_SECRET
echo "whsec_$(openssl rand -base64 32)"         # -> GITLAB_WEBHOOK_SIGNING_SECRET
# (or append from the shell, which DOES run the substitution first:
#  echo "REVIEW_API_TOKEN=$(openssl rand -hex 32)" >> .env)

# Then paste the outputs into .env:
REVIEW_API_TOKEN=<hex output>

# GitLab EE Personal Access Token
# Create at: https://your-gitlab.example.com/-/profile/personal_access_tokens
# Required scopes: api, read_repository
GITLAB_TOKEN=glpat-xxxxxxxxxxxxxxxxxxxx

# Legacy webhook secret (any random string)
GITLAB_WEBHOOK_SECRET=<hex output>

# Signing token (recommended, GitLab 19.0+). MUST start with `whsec_` followed
# by base64 — a plain hex string is silently treated as invalid.
GITLAB_WEBHOOK_SIGNING_SECRET=whsec_<base64 output>

# LLM configuration (JSON)
LLM_CONFIG='[{"provider":"openai","model":"gpt-4o","api_key":"sk-..."}]'

# Your GitLab EE URL
GITLAB_URL=https://gitlab.example.com
```

> **Port:** The service maps host port **18080** to the container's 8080 by
> default (`"${REVIEW_ENGINE_PORT:-18080}:8080"` in `docker-compose.yml`). To
> use a different port, set `REVIEW_ENGINE_PORT` in `.env` and use that value
> in every URL and curl below.

### 3. Start the Service

```bash
docker compose up -d
```

> **First build:** the Dockerfile is multi-stage and compiles the Rust backend
> **and** the Vue frontend inside the image — no manual `npm run build` is
> needed. The first build can take **10–30 minutes** depending on your network
> and CPU; later rebuilds are faster thanks to layer caching.

> **Logs:** app logs are written *inside the container* to
> `$HOME/.config/review-engine/logs.ndjson` (not to docker stdout), so
> `docker compose logs` only shows startup output. Follow the app log with:
> ```bash
> docker compose exec review-engine sh -c 'tail -f "$HOME/.config/review-engine/logs.ndjson"'
> ```
> …or open the **Logs** page in the Web UI at `http://localhost:18080`.

### 4. Verify Health

```bash
curl http://localhost:18080/health
# Expected: {"status":"ok"}
```

---

## 🔗 GitLab EE Webhook Configuration

### Option A: Project-Level Webhook (Recommended)

1. Go to **Project → Settings → Webhooks**
2. Add URL: `http://<your-server-ip>:18080/webhook/gitlab`
3. Set **Secret Token**: the value from `GITLAB_WEBHOOK_SECRET` (legacy, optional)
4. Set **Signing Token**: the value from `GITLAB_WEBHOOK_SIGNING_SECRET` (recommended, GitLab 19.0+)
   - More secure than Secret Token: HMAC-SHA256 of the request body
   - Review-Engine verifies the `webhook-signature` header (Standard Webhooks — note the header has no `X-` prefix)
5. Select triggers:
   - ✅ **Merge request events**
   - ✅ **Comments** (optional, for re-trigger)
6. Save and test with "Test → Merge request events"

> **Note:** You can configure both Secret Token and Signing Token for defense-in-depth. When a `webhook-signature` header is present, Review-Engine verifies **only the signature** — a failing signature is rejected without falling back to the legacy token (this prevents downgrade attacks). The legacy `X-Gitlab-Token` check is used only when the `webhook-signature` header is absent.

### Option B: Group-Level Webhook (All Projects)

1. Go to **Group → Settings → Webhooks**
2. Same URL and secret as above
3. Applies to all projects in the group

### Option C: System-Level Hook (Admin Only)

1. Go to **Admin → System Hooks**
2. URL: `http://<your-server-ip>:18080/webhook/gitlab`
3. Enable **Merge request events**

---

## 🔒 Security Checklist

| Item | Status | How |
|------|--------|-----|
| API token set | ☐ | `REVIEW_API_TOKEN` in `.env` |
| API auth enforced | ☐ | `curl -i http://<server>:18080/api/v1/system/version` without a token should return `401 Unauthorized` |
| Webhook secret (legacy) set | ☐ | `GITLAB_WEBHOOK_SECRET` in `.env` (optional) |
| Webhook signing token set | ☐ | `GITLAB_WEBHOOK_SIGNING_SECRET` in `.env` (recommended, GitLab 19.0+) |
| HTTPS enabled | ☐ | Use Caddy/Nginx reverse proxy |
| Firewall rules | ☐ | Only expose the host port (default 18080) to GitLab EE |
| Token rotation | ☐ | Rotate every 90 days |

---

## 🐛 Troubleshooting

### Webhook not triggering

```bash
# App logs are written inside the container (not to docker stdout), so grep the
# log file directly — or use the Logs page in the Web UI:
docker compose exec review-engine sh -c 'grep -i "webhook\|gitlab" "$HOME/.config/review-engine/logs.ndjson"'

# Verify GitLab can reach your server
curl -v http://<your-server-ip>:18080/webhook/gitlab -X POST
```

### Signing token verification fails

If you configured `GITLAB_WEBHOOK_SIGNING_SECRET` but webhooks return 403:

```bash
# Check that the signing secret matches GitLab's signing token
docker compose exec review-engine sh -c 'grep -i "signing\|signature\|mismatch" "$HOME/.config/review-engine/logs.ndjson"'

# Verify both headers are present (if using both methods).
# Note: GitLab sends `webhook-signature` (no X- prefix), format "v1,<base64-hmac>".
curl -v http://<your-server-ip>:18080/webhook/gitlab \
  -X POST \
  -H "X-Gitlab-Token: $GITLAB_WEBHOOK_SECRET" \
  -H "webhook-signature: v1,<base64-hmac>" \
  -d '{"test":"body"}'
```

### LLM errors

```bash
# Precondition: `LLM_CONFIG` must be exported in THIS shell — variables in .env
# are only loaded by docker compose, not into your shell. For example:
#   export LLM_CONFIG='[{"provider":"openai","model":"gpt-4o","api_key":"sk-..."}]'
python3 -c "import json; json.loads('''$LLM_CONFIG''')"

# Check logs for API errors
docker compose exec review-engine sh -c 'grep -i "llm\|error" "$HOME/.config/review-engine/logs.ndjson"'
```

### Out of memory

```bash
# Increase memory limit in docker-compose.yml
deploy:
  resources:
    limits:
      memory: 4G
```

---

## 📊 Monitoring

### Prometheus Metrics

Available at `http://localhost:18080/metrics`:

| Metric | Description |
|--------|-------------|
| `review_duration_seconds` | Review execution time |
| `review_findings_total` | Number of findings per review |
| `llm_requests_total` | LLM API call count |
| `webhook_requests_total` | Webhook request count |

### Health Check

```bash
curl http://localhost:18080/health
```

---

## 🔄 Updates

```bash
# Pull latest code
git pull origin main

# Rebuild and restart
docker compose down
docker compose up -d --build
```

---

## 📁 File Structure

```
ReviewEngine/
├── .env                 # Your environment config (gitignored)
├── docker-compose.yml   # Docker Compose orchestration
├── Dockerfile           # Multi-stage build
├── config/              # Config files (mounted volume)
│   └── .code-audit-config.toml
└── reports/             # Review outputs (mounted volume)
```

---

## 🆘 Getting Help

- **GitHub Issues**: https://github.com/Liewzheng/ReviewEngine/issues
- **Webhook Events**: See the **Logs** page in the Web UI, or `docker compose exec review-engine sh -c 'tail -f "$HOME/.config/review-engine/logs.ndjson"'`
- **GitLab Docs**: https://docs.gitlab.com/ee/user/project/integrations/webhooks.html
