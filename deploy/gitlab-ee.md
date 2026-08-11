# Deploy Review-Engine for GitLab EE (Self-Hosted)

This guide walks you through deploying **review-engine** as a Docker container for GitLab Enterprise Edition (self-hosted) code review automation.

---

## 📋 Prerequisites

- Docker Engine ≥ 24.0 + Docker Compose v2
- GitLab EE instance with admin or project owner access
- LLM API keys (OpenAI, Anthropic, or compatible)
- A server with ≥ 2 CPU cores, 2 GB RAM, 10 GB disk

---

## 🚀 快速部署(无需 Clone,pull 镜像)——推荐

不需要 `git clone` 整个仓库:只需 **一个镜像 + 一份独立 compose 文件** 即可部署。
镜像内含零编译二进制、前端 dist、首次同步与容器内自更新(UI 点 Upgrade 即可升级)。

### 1. 拉取镜像

```bash
docker pull ghcr.io/liewzheng/review-engine:latest
```

> 镜像为多架构(amd64/arm64),由发版流水线自动推送 GHCR(ghcr.io/liewzheng/review-engine)。
> 要锁版本用具体 tag(如 `:v0.9.9`);`:latest` 跟随最新发版。

### 2. 准备独立 compose 文件

`deploy/standalone-compose.yml` 是自包含的单文件(不依赖仓库其他文件),把它放到自己的部署目录:

```bash
mkdir -p ~/review-engine && cd ~/review-engine
# 把 deploy/standalone-compose.yml 复制/下载到当前目录(可命名为 docker-compose.yml)
```

### 3. (可选)配置认证方式

镜像默认走 **bootstrap 首次引导**:容器绑定 0.0.0.0(非 loopback),
若不在环境里设 `REVIEW_API_TOKEN`,**必须**给一次性 `REVIEW_BOOTSTRAP_KEY`,否则服务拒绝启动:

```bash
echo "REVIEW_BOOTSTRAP_KEY=$(openssl rand -hex 16)" >> .env
```

> 也可以直接在 .env 设 `REVIEW_API_TOKEN=xxxx`(env 注入,兼容旧版,优先于 UI 设置)。
> 两种都不设 → 服务启动失败并提示(见容器日志),补上即可。

### 4. 启动并验证

```bash
docker compose up -d
docker compose ps                  # 等待 STATUS 变 (healthy)
curl http://localhost:18080/health # 期望 {"status":"ok"}
```

所有卷目录(`config/ reports/ bin/ frontend-dist/ auth/`)自动相对当前目录创建,配置与代码分离。

### 5. 首次 UI 引导

打开 `http://<宿主IP>:18080`:

- 首次进入按提示设置 **API token**(若用了 bootstrap key,页面会要求输入它作为一次性凭证);
- 在 **Configuration** 页填写 GitLab EE 地址/token、webhook、LLM 配置(也可用环境变量注入,见下);
- token 持久化到 `./auth/auth.toml`(**SHA-256 摘要,非明文**),设置完成后可从 .env 删掉 `REVIEW_BOOTSTRAP_KEY`。

> **Linux NAS 提示**:bind 卷属主继承宿主,若容器写卷失败(如 "Permission denied"),
> 先 `mkdir -p config reports bin frontend-dist auth && chown -R <容器UID> *`
> (UID 以 `docker exec <容器名> id review-engine` 为准,典型 999)。

### 6. 日常运维

- **升级**:UI 右上角 Upgrade(容器内自更新——自动替换 `./bin` 与 `./frontend-dist` 卷并重启,无需重新 pull/重建);
- **HTTPS**:取消 standalone-compose.yml 中 `443:8443` 端口与 `./tls` 卷注释,放好 `./tls/cert.pem`、`./tls/key.pem`,设 `REVIEW_TLS_CERT`/`REVIEW_TLS_KEY`;
- **可选 env 注入**(也可全部在 UI Configuration 页填):`REVIEW_API_TOKEN`、`GITLAB_URL`、`GITLAB_TOKEN`、`GITLAB_WEBHOOK_SECRET`、`GITLAB_WEBHOOK_SIGNING_SECRET`、`LLM_CONFIG`。

---

## 🔧 源码构建部署(需要 Clone,高级/可选)

> 以下为从源码构建镜像的方式(需要 `git clone` 整个仓库);不需要 clone 的部署
> 见上方"🚀 快速部署(无需 Clone)"章节。两套部署互不冲突(镜像名/卷路径不同)。

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

### 1.5 部署配置与代码分离 (Deploy Config vs Code)

`.env`, `config/`, `reports/` and `tls/` are **runtime deploy state**, not
source code. `.env` is **not tracked** by git (see `.gitignore`), so a plain
`git pull` on the deploy machine never conflicts with local credentials. Keep
the code repository pristine — clone once, then `git pull` + rebuild to
upgrade — and keep all credentials/data in a separate, independently-backed-up
deploy directory.

**Recommended: put runtime config in a repo-external deploy directory**
(e.g. `/volume1/docker/reng/` on a Synology NAS):

```text
/volume1/docker/reng/
├── .env          # real credentials (copy from .env.example, then edit)
├── config/       # review-engine config → mounted to /app/config (read-only)
├── reports/      # review reports     → mounted to /app/reports
└── tls/          # TLS cert/key for native HTTPS (optional) → mounted to /app/tls
```

Point the compose volume variables at that directory with **absolute paths**
inside the deploy `.env`:

```bash
CONFIG_PATH=/volume1/docker/reng/config
REPORTS_PATH=/volume1/docker/reng/reports
SSH_KEY_PATH=/volume1/docker/reng/.ssh   # or keep your existing ~/.ssh
```

Then start compose from the **code repository** but load the deploy `.env`
explicitly:

```bash
cd ReviewEngine
git pull && docker compose build review-engine
docker compose --env-file /volume1/docker/reng/.env up -d --force-recreate review-engine
```

The repo working tree stays clean (`git status` shows no runtime files), and
the deploy directory holds every credential and data file — back it up
independently of the code.

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

## 🔐 HTTPS Deployment

`review-engine` ships with native TLS support (`reng serve` accepts
`--tls-cert`, `--tls-key`, and `--tls-port`) — **no reverse proxy needed**.
HTTPS is the default external transport (port 443). HTTP still works: container
port 8080 stays reserved for health checks (`http://localhost:8080/health`), and
external plain HTTP is gated by the `REVIEW_ENABLE_HTTP` switch (default
**off**, so host port 18080 is not exposed).

> **Port map (quick reference)**
>
> | Port | Purpose |
> |------|---------|
> | `443` (`REVIEW_TLS_PORT`, default) | External HTTPS entry, maps to container port `8443` |
> | `18080` (`REVIEW_ENGINE_PORT`, default) | External HTTP direct-connect — only when `REVIEW_ENABLE_HTTP=1`; maps to container port `8080` |
> | `8080` (container) | Health check, always available, unaffected by TLS |

### Option A: Docker (Recommended)

**1. Generate a certificate**

This creates a self-signed certificate — fine for internal / self-hosted use.
For public-facing services, use a certificate from a trusted CA instead.

```bash
mkdir -p tls
openssl req -x509 -nodes -newkey rsa:2048 \
  -keyout tls/key.pem \
  -out tls/cert.pem \
  -days 365 \
  -subj "/CN=<your-server-ip-or-domain>"
```

**2. Configure `.env`**

```bash
# Host-side paths to the TLS certificate and private key. When set, Compose
# mounts them into the container at /app/tls/ and enables TLS via the
# REVIEW_TLS_CERT / REVIEW_TLS_KEY environment variables.
TLS_CERT_PATH=./tls/cert.pem
TLS_KEY_PATH=./tls/key.pem

# External HTTPS host port (default 443). Only needed for a non-standard port.
# REVIEW_TLS_PORT=8443

# Optional: expose plain HTTP on host port 18080 for internal direct access.
# Default is off — the service is HTTPS-only externally.
# REVIEW_ENABLE_HTTP=1
```

**3. Start and verify**

```bash
docker compose up -d
curl -k https://<your-server-ip>/health
# Expected: {"status":"ok"}
```

> - **Fail-soft:** without `TLS_CERT_PATH` / `TLS_KEY_PATH` (or if the cert
>   files are missing), the service falls back to plain HTTP, matching older
>   behavior.
> - If you set a custom `REVIEW_TLS_PORT`, verify with
>   `curl -k https://<your-server-ip>:<REVIEW_TLS_PORT>/health`.
> - `-k` (`--insecure`) skips self-signed cert validation; in production prefer
>   a trusted certificate and drop `-k`.

### Option B: Bare Binary

```bash
reng serve \
  --tls-cert tls/cert.pem \
  --tls-key tls/key.pem \
  --tls-port 8443
```

- Native HTTPS is enabled only when **both** `--tls-cert` and `--tls-key` are
  provided; the listener binds to `--tls-port` (8443 in the example).
- Without certificates the server stays on plain HTTP (fail-soft) — no config
  change needed to fall back.

Verify:

```bash
curl -k https://localhost:8443/health
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

> **Webhook URL scheme (HTTPS vs HTTP):** With HTTPS enabled (the default), use
> `https://<your-server-ip>/webhook/gitlab` — port 443, no port suffix. The
> `http://<your-server-ip>:18080/webhook/gitlab` form (Options A–C above) only
> works when HTTP direct-connect is enabled: set `REVIEW_ENABLE_HTTP=1` in
> `.env` and restart with `docker compose up -d`. Option B and C use the same
> URL scheme as Option A.

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
| HTTPS enabled | ☐ | Built-in TLS: mount certs in Docker (`TLS_CERT_PATH` / `TLS_KEY_PATH`) or run `reng serve --tls-cert/--tls-key`; a reverse proxy also works |
| Firewall rules | ☐ | Only expose what GitLab EE needs: 443 (HTTPS, default) and 18080 only if HTTP direct-connect is on (`REVIEW_ENABLE_HTTP=1`) |
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
