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

# Copy environment template
cp .env.example .env

# Edit .env with your credentials
nano .env
```

### 2. Required Environment Variables

Edit `.env` and set these **required** variables:

```bash
# Generate a secure API token
REVIEW_API_TOKEN=$(openssl rand -hex 32)

# GitLab EE Personal Access Token
# Create at: https://your-gitlab.example.com/-/profile/personal_access_tokens
# Required scopes: api, read_repository
GITLAB_TOKEN=glpat-xxxxxxxxxxxxxxxxxxxx

# Webhook secret (any random string)
GITLAB_WEBHOOK_SECRET=$(openssl rand -hex 32)

# LLM configuration (JSON)
LLM_CONFIG='[{"provider":"openai","model":"gpt-4o","api_key":"sk-..."}]'

# Your GitLab EE URL
GITLAB_URL=https://gitlab.example.com
```

### 3. Start the Service

```bash
docker compose up -d

# Check logs
docker compose logs -f review-engine
```

### 4. Verify Health

```bash
curl http://localhost:8080/health
# Expected: {"status":"ok"}
```

---

## 🔗 GitLab EE Webhook Configuration

### Option A: Project-Level Webhook (Recommended)

1. Go to **Project → Settings → Webhooks**
2. Add URL: `http://<your-server-ip>:8080/webhook/gitlab`
3. Set **Secret Token**: the value from `GITLAB_WEBHOOK_SECRET`
4. Select triggers:
   - ✅ **Merge request events**
   - ✅ **Comments** (optional, for re-trigger)
5. Save and test with "Test → Merge request events"

### Option B: Group-Level Webhook (All Projects)

1. Go to **Group → Settings → Webhooks**
2. Same URL and secret as above
3. Applies to all projects in the group

### Option C: System-Level Hook (Admin Only)

1. Go to **Admin → System Hooks**
2. URL: `http://<your-server-ip>:8080/webhook/gitlab`
3. Enable **Merge request events**

---

## 🔒 Security Checklist

| Item | Status | How |
|------|--------|-----|
| API token set | ☐ | `REVIEW_API_TOKEN` in `.env` |
| Webhook secret set | ☐ | `GITLAB_WEBHOOK_SECRET` in `.env` |
| HTTPS enabled | ☐ | Use Caddy/Nginx reverse proxy |
| Firewall rules | ☐ | Only expose 8080 to GitLab EE |
| Token rotation | ☐ | Rotate every 90 days |

---

## 🐛 Troubleshooting

### Webhook not triggering

```bash
# Check if webhook handler is registered
docker compose logs review-engine | grep -i "webhook\|gitlab"

# Verify GitLab can reach your server
curl -v http://<your-server-ip>:8080/webhook/gitlab -X POST
```

### LLM errors

```bash
# Check LLM config is valid JSON
python3 -c "import json; json.loads('''$LLM_CONFIG''')"

# Check logs for API errors
docker compose logs review-engine | grep -i "llm\|error"
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

Available at `http://localhost:8080/metrics`:

| Metric | Description |
|--------|-------------|
| `review_duration_seconds` | Review execution time |
| `review_findings_total` | Number of findings per review |
| `llm_requests_total` | LLM API call count |
| `webhook_requests_total` | Webhook request count |

### Health Check

```bash
curl http://localhost:8080/health
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
- **Webhook Events**: Check `docker compose logs -f`
- **GitLab Docs**: https://docs.gitlab.com/ee/user/project/integrations/webhooks.html
