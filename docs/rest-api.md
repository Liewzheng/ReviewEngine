---
title: REST API 设计方案
description: 为 review-engine 添加面向前端的 REST API 层，支持 Web UI、桌面 App、VSCode Extension
tags:
  - rest-api
  - architecture
  - frontend
related:
  - ../.notes/review_engine_rs_roadmap.md
  - ../src/server/mod.rs
---

# REST API 设计方案

> 目标：在现有 Rust 核心 + CLI 基础上，新增 REST API 层，让前端方案（Web UI / 桌面 App / VSCode Extension）可通过 HTTP 调用 review-engine 的全部能力。

---

## 架构概览

```
┌──────────────────────────────────────────────────────────┐
│                      Frontend Layer                      │
│   Web UI (React/Vue)  │  Desktop App  │  VSCode Extension │
│          ╲                  │                ╱             │
│           ╲     HTTP REST + SSE（可选）      ╱              │
│            ╲                 │              ╱               │
├──────────────────────────────────────────────────────────┤
│                    API Layer（新增）                        │
│  src/server/api/                                          │
│  ├── mod.rs       路由注册 + CORS                        │
│  ├── review.rs    POST/GET/DELETE review 任务             │
│  ├── config.rs    GET/PUT config, schema, validate, test, models │
│  ├── system.rs    health, version, experts list/update     │
│  ├── queue.rs     queue stats, tasks, pause/resume/retry   │
│  ├── llm.rs       LLM providers CRUD + connectivity test   │
│  ├── logs.rs      日志 SSE 流 + 下载                      │
│  ├── dashboard.rs Dashboard KPI/趋势/健康聚合              │
│  ├── events.rs    SSE 实时推送                            │
│  └── types.rs     TaskStatus, PaginatedResponse 等         │
│                                                           │
│  src/server/task_queue.rs   异步任务队列 + TaskStore      │
│  src/server/auth.rs         Bearer token 中间件           │
├──────────────────────────────────────────────────────────┤
│                    Rust Core（现存）                       │
│  orchestrator · diff · llm · output · team · tools        │
└──────────────────────────────────────────────────────────┘
```

### 设计原则

1. **复用核心，不重复逻辑** — API 层只做 HTTP 路由 + 序列化，所有业务逻辑走现有 `crate::orchestrator`、`crate::output`、`crate::models`、`crate::repo`
2. **异步优先** — review 涉及 LLM 调用（10-60s），全部走 task 模型：提交→返回 task_id→轮询/推送结果
3. **前端无关** — 只输出结构化 JSON，不耦合任何前端框架
4. **自描述** — `/api/v1/config/schema` 输出 JSON Schema，前端可动态渲染配置表单

---

## 端点参考

### 1. Reviews

提交 review 任务（异步），支持三种 source：

| 字段 | 类型 | 说明 |
|------|------|------|
| `source.type` | `"gitlab_mr"` \| `"local_repo"` \| `"static_diff"` | 输入源类型 |
| `source.url` | `string` | GitLab MR URL（仅 `gitlab_mr`） |
| `source.path` | `string` | 本地仓库路径（仅 `local_repo`） |
| `source.base` | `string` | base ref（仅 `local_repo`，默认 `main`） |
| `source.head` | `string` | head ref（仅 `local_repo`） |
| `source.diff` | `string` | 原始 diff 文本（仅 `static_diff`） |
| `config` | `string` | 可选 TOML 配置，覆盖默认 |
| `llm_configs` | `LLMConfig[]` | 可选 LLM 配置覆盖 |
| `webhook` | `string` | 可选回调 URL，完成后 POST 结果；必须通过下方「Webhook 回调 URL 校验」 |

> **凭证传输（安全要求）**：请求体**不得**携带任何敏感 token。`gitlab_mr` 所需的 GitLab
> token 必须通过请求头 `X-Gitlab-Token` 传输（原 `source.token` 字段已移除）；该头承载
> GitLab 上游凭证，与 §7 的 API 鉴权头 `Authorization: Bearer` / `X-API-Key` 相互独立，
> 同一请求可同时携带两者（注意区分：`/webhook/gitlab` 入站回调上的同名头承载的是 webhook
> secret，与此处含义不同，两者互不影响）。请求头缺失时，服务端回退使用服务器侧已配置的
> GitLab token（优先 Web UI **Git 平台** 条目 / `ui-state.toml`，按 MR URL 的 `host[:port]`
> 匹配该条目的 `base_url` 或已配置的 `internal_base_url`；`--gitlab-token` /
> `GITLAB_TOKEN` 已降级为 fallback-only，仅在服务器侧无该值时生效并打 deprecation 警告）；都缺失时
> 返回 `400`。token 永远不会在响应或日志中返回（遵循 `***` 掩码约定，见 §3）。
> `llm_configs` 中的 `api_key` 同属敏感字段：只允许经 §7 已认证的 `/api/v1` 通道提交，
> 同样不得回显。

#### `POST /api/v1/reviews`

```
Request:
  POST /api/v1/reviews
  Authorization: Bearer review_a1b2...    # §7 API token（非 loopback 绑定时必需）
  X-Gitlab-Token: glpat-xxx               # GitLab 上游凭证（仅 gitlab_mr；不得写入请求体）
  Content-Type: application/json

{
  "source": {
    "type": "gitlab_mr",
    "url": "https://gitlab.com/owner/repo/-/merge_requests/23"
  },
  "config": null,
  "llm_configs": [],
  "webhook": null
}

Response 202:
{
  "task_id": "550e8400-e29b-41d4-a716-446655440000",
  "status": "pending",
  "created_at": "2026-06-26T12:00:00Z",
  "_links": {
    "self": "/api/v1/reviews/550e8400-e29b-41d4-a716-446655440000"
  }
}
```

#### `gitlab_mr` URL 的主机改写与不可达主机拒绝（RENG-33）

手动提交的 `source.url` 通常是调用方浏览器能打开的地址（即 GitLab 的 `external_url`），而 review-engine 自己（常常跑在容器里）未必能访问它——容器内的 `localhost` 指向容器自身。webhook 路径早已按「匹配到的 Git 平台」把 payload URL 改写到可达地址（见 `docs/integrations/gitlab.md` 的 Internal URL 一节）；REST 提交路径自 0.10.15 起遵循同一套规则、复用同一个改写函数 `rewrite_url_to_platform`：

1. **主机匹配**：把提交 URL 的 `host[:port]` 身份（scheme 不参与、host 大小写不敏感、显式写出的默认端口 80/443 折叠为「未写」、其余端口严格比对）与每个 Git 平台条目比对，命中其 `base_url` 或（已配置的）`internal_base_url` 即视为同一实例；按配置顺序取第一个命中项。URL 的路径、查询串、尾部 `/` 都不参与匹配。
2. **改写**：命中后，提交 URL 的路径与查询串被重新挂到该平台的可达地址（`internal_base_url`，未配置则 `base_url`）。改写后的 URL 既是异步评审实际抓取的地址，也是任务记录里保存的 MR URL（`GET /api/v1/reviews/:task_id` 的 `gitlabMrUrl` 因此始终是「实际抓取的那个地址」）；调用方提交的原始 URL 不入库，仅在服务端日志中与命中的平台名一起记录一次。
3. **未命中且为本地地址**：没有任何平台命中、且 URL 主机是众所周知的本地地址（`localhost`、`*.localhost`、任意 `127.0.0.0/8`、`0.0.0.0`、`::1`、`::`）时，**在入队之前**以 `400` 拒绝。这类地址在容器内指向容器自身，放行只会得到一个晚到的、含义不明的 `Failed to send GET`：

```
Response 400:
{
  "error": "gitlab_mr url host `localhost` is unreachable from the review server: a local address names the server itself (inside a container, the container), and no configured git platform matches it. Use the GitLab address this server can reach — in Docker that is usually `host.docker.internal` (e.g. `http://host.docker.internal:8929/group/project/-/merge_requests/1`) — or configure a git platform entry for this host with `baseUrl` = the address you browse to and `internalBaseUrl` (`internal_base_url`) = the address the server reaches, so MR URLs on this host are rewritten onto the reachable base automatically"
}
```

   消息里直接给出两条出路：改用服务端真正能访问的地址（容器内通常是 `host.docker.internal`），或为该主机配置一个 Git 平台条目（`baseUrl` = 浏览器里的地址，`internalBaseUrl` = 服务端可达地址），让第 2 条改写自动生效。
4. **未命中且非本地地址**：原样提交、原样抓取（如 `https://gitlab.com/...`）——服务端能否访问它不由本服务臆断。
5. **命中即信任**：平台是部署方的显式配置，命中后其配置的可达地址总被采信（因此 GitLab 确实与 review-engine 同机、只在本地地址可达的部署仍然可用）。

**错误码边界**：URL 解析失败仍是 `422 invalid gitlab_mr url: ...`（解析校验先于主机路由）；主机路由失败才是上面的 `400`。两者都发生在 `enqueue_review` 之前，因此都不会在评审历史里留下 `Untitled Review` 记录。`POST /api/v1/reviews/:task_id/rerun` 重放存储参数时应用同一套校验。

**已入队后才失败的记录**：入队之前能判定的失败不建记录；入队之后才失败的评审（主机可达但认证失败、解析成功却抓取 diff 失败等）仍保留一条 `failed` 记录——那是调用方唯一能看到失败原因的地方，也是修好配置后 `rerun` 的入口，因此不做清理（清理会连带丢掉「可重跑」这一恢复路径）。

#### `GET /api/v1/reviews/:task_id`

返回单个任务详情。`task_id` 必须是合法 UUID：非 UUID 值在路径参数解析阶段即失败，返回 `400`（如误请求 `/api/v1/reviews/history`——历史列表端点是下方单独的 `GET /api/v1/reviews`，不存在 `history` 子路径）；任务不存在返回 `404 { "error": "task not found" }`。

snake_case `TaskStatus` 字段全部保留，之上合并 camelCase 结构化字段（`ReviewDetail`）：`id` / `mrTitle` / `project` / `repository` / `branch` / `targetBranch` / `author{name, avatarUrl}` / `participants[]` / `status` / `durationMs` / `createdAt` / `completedAt` / `commitSha` / `experts[{expertId, expertName, status, score, summary, details}]` / `rawComment` / `rawApiResponse` / `gitlabMrUrl`。

```
Response 200:
{
  "task_id": "550e8400-e29b-41d4-a716-446655440000",
  "status": "completed",           // pending | running | completed | failed | cancelled
  "created_at": "2026-06-26T12:00:00Z",
  "completed_at": "2026-06-26T12:00:28Z",
  "duration_ms": 28400,
  "result": {
    "reports": [
      {
        "expert_name": "Security",
        "findings": [
          {
            "file": "src/auth.rs",
            "line": 42,
            "severity": "high",
            "confidence": 9,
            "title": "SQL Injection Risk",
            "summary": "User input concatenated into query",
            "evidence": "...",
            "recommendation": "Use parameterized queries",
            "effort": "trivial",
            "expert_name": "Security",
            "expert_role": "Security Lead"
          }
        ],
        "metrics": {
          "latency_ms": 12000,
          "tokens_used": 4500
        }
      }
    ],
    "aggregated": null
  },
  "error": null,
  "mr_title": "Fix login",                    // 原 snake_case MR 元数据字段全部保留
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "mrTitle": "Fix login",
  "project": "owner/repo",
  "repository": "owner/repo",
  "branch": "feature/x",
  "targetBranch": "main",
  "author": { "name": "alice", "avatarUrl": "https://gitlab.com/avatar.png" },
  "participants": [
    { "name": "Alice", "username": "alice", "avatarUrl": "https://gitlab.com/alice.png", "role": "author", "bot": false },
    { "name": "Bob", "username": "bob", "avatarUrl": null, "role": "creator", "bot": false },
    { "name": "Group Bot", "username": "group_1_bot", "avatarUrl": null, "role": "participant", "bot": true }
  ],
  "durationMs": 28400,
  "createdAt": "2026-06-26T12:00:00Z",
  "completedAt": "2026-06-26T12:00:28Z",
  "commitSha": "abc123",
  "experts": [
    {
      "expertId": "security",
      "expertName": "Security",
      "status": "success",
      "score": 9,
      "summary": "## Security review ...",
      "details": "raw LLM response ..."
    }
  ],
  "rawComment": "aggregated markdown ...",
  "rawApiResponse": { "reports": [ ... ] },
  "gitlabMrUrl": "https://gitlab.com/owner/repo/-/merge_requests/23"
}
```

**`participants` 字段（RENG-42/44）**

详情与列表项都带 `participants`，元素形状固定为：

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `name` | string | 显示名；provider 没有显示名时回退为 `username` |
| `username` | string | provider 句柄（GitLab username / GitHub login）；未知时为 `""`（不是 `null`） |
| `avatarUrl` | string \| null | 头像 URL；provider 未返回时为 `null` |
| `role` | string | `"author"` / `"creator"` / `"participant"` |
| `bot` | boolean | 机器人账号（GitLab `*_bot` 账号或显式 bot 标记；GitHub `type: "Bot"`） |

取值与排序规则：

- `author` = head commit 的作者（真正写代码的人）；`creator` = MR/PR 开启人；`participant` = 其余参与者（评论者、审核者、机器人）。
- 顺序固定为 `author` → `creator` → `participant`；同一角色内保持 provider 返回的顺序。
- 按人去重（用户 id 优先，其次 username，再次显示名），同一个人只保留优先级最高的一条（`author` > `creator` > `participant`）；不同机器人是不同主体，**不合并**。
- 列表项与详情项使用同一份数据、同一套顺序，跨页一致。
- 数据来源为 best-effort：provider 的 participants 接口失败、或某来源查不到时，只是列表变短，绝不会让评审失败。
- **兼容性**：0.10.6 之前的旧记录没有该字段，返回 `[]`（不是 `null`、不报错）。旧字段 `author{name, avatarUrl}` 保留不删。

#### `GET /api/v1/reviews`

分页列出历史 reviews。这是唯一的 review 历史列表端点（前端 History 页的数据源）；不存在 `/api/v1/reviews/history` 子路径——该请求会命中 `/:task_id` 路由并因 `history` 不是 UUID 而返回 400。

```
Query:
  ?page=1&per_page=20&status=completed

Response 200:
{
  "items": [ ... ],
  "total": 42,
  "page": 1,
  "per_page": 20
}
```

Default `per_page`: 20, max `per_page`: 100. When `page` exceeds range, returns empty `items` with correct `total`.

`status` 过滤支持 `pending` / `running` / `completed` / `failed` / `cancelled`。

列表项在 snake_case `TaskStatus` 字段之上合并轻量 camelCase 字段（`id` / `mrTitle` / `project` / `repository` / `branch` / `targetBranch` / `author{name, avatarUrl}` / `participants[]` / `status` / `durationMs` / `createdAt` / `gitlabMrUrl`）；detail 才有的 `experts` / `rawComment` / `rawApiResponse` 不会出现在列表项中。`participants` 的字段形状、取值、排序与去重规则见上方「`participants` 字段（RENG-42/44）」，与详情项完全一致（旧记录同样返回 `[]`）。

#### `DELETE /api/v1/reviews/:task_id`

将 `pending` / `running` 状态的 task 迁移为 `cancelled`：状态迁移而非物理删除，记录保留、可继续通过 `GET` 查询。`completed` / `failed` / 已 `cancelled` 的任务返回 `400`。

```
Response 200: { "status": "deleted" }
Response 400: { "error": "task not found or cannot be cancelled" }
```

#### `POST /api/v1/reviews/:task_id/rerun`

用原任务的请求参数（source / config / llm_configs / webhook）重新创建任务并排入队列，返回新任务 id；原任务记录不变。

存储的请求参数不含 GitLab token（凭证走 `X-Gitlab-Token` 请求头，不随请求参数落存储）；rerun 时按同一凭证传输规则重新解析——调用方可重新携带该头，否则回退服务器侧已配置的 GitLab token。

```
Response 202:
{
  "task_id": "9c7f2d1e-b3a4-4c5d-8e6f-7a8b9c0d1e2f"
}

Response 404: { "error": "task not found" }
Response 409: { "error": "task is still running" }
Response 409: { "error": "original request parameters are not available" }
Response 422: { "error": "stored request parameters are not replayable" }
```

- `404`：任务不存在
- `409`：任务仍处于 `pending` / `running`
- `409`：原任务未保存请求参数（不可回放）
- `422`：保存的请求参数无法反序列化为 `ReviewRequest`（参数不可回放）

#### Webhook 回调 URL 校验（SSRF 防护）

`webhook` 字段让服务端在任务完成后向调用方指定的任意 URL 主动发起 POST，相当于一个由 API 调用方控制的服务端出站请求，是典型的 SSRF 攻击面：不加限制时，攻击者既可把任务结果（含 findings 摘要）外发到恶意端点，也可借服务端身份探测内网拓扑、访问云厂商元数据接口（如 `169.254.169.254`）。因此服务端**必须**在入队前校验回调 URL，校验失败即拒绝整个请求（`400` + `{"error": "invalid webhook url: ..."}`），不得静默忽略：

1. **Scheme 白名单**：仅允许 `https`。`http` 仅当部署方显式声明为 loopback / 内网部署时放行（目标须为 `127.0.0.0/8`、`::1`、`10.0.0.0/8`、`172.16.0.0/12`、`192.168.0.0/16` 等 loopback / 私有地址）；`file:`、`ftp:`、`gopher:` 等其余 scheme 一律拒绝。
2. **链路本地 / 元数据地址拒绝**：目标 IP（含域名解析结果，防 DNS rebinding 指向内网）不得落在 link-local 段 `169.254.0.0/16`（含云元数据地址 `169.254.169.254`）、`0.0.0.0/8`、IPv6 `fe80::/10` 等保留段；跟随重定向时对重定向后的地址须重新校验。
3. **可选 host 白名单**：部署方可配置回调 host 白名单（例如仅允许企业内部网关域名）；配置后白名单外的 host 一律拒绝。

---

### 2. 仓库健康扫描

与 review 共用同一套 task 机制（`TaskStore` 队列 + 进度跟踪）。扫描只走服务器本地文件系统：未配置 LLM 时运行纯静态专家分析（`run_local_repo_review`），不依赖外部 LLM，可快速返回；配置了 LLM 时自动走 LLM 增强的 3-pass 流水线（`run_repo_review`）。

#### `POST /api/v1/repo-scan`

```
Request:
{
  "path": "/path/to/repo"   // 必填，服务器本地目录路径（允许绝对路径）
}

Response 202:
{
  "task_id": "...",
  "status": "pending",       // 入队后为 pending，获得执行槽位后转 running
  "created_at": "...",
  "result": null,
  "error": null,
  ...                        // 其余字段同 TaskStatus
}

Response 400:   // 路径校验失败（路径为空 / 含 '..' / 不存在 / 不是目录）
{ "error": "path does not exist: ..." }

Response 503:   // task store 未初始化
{ "error": "task store not initialized" }
```

#### `GET /api/v1/repo-scan/:task_id`

返回 `TaskStatus`（同 reviews 端点）：`status` 为 `pending` / `running` / `completed` / `failed`；`completed` 时 `result` 为 `RepoReviewOutput` JSON（含 `overview.health_score`、`expert_scores`、`risk_categories`、`action_items`、`conclusion` 等），`failed` 时 `error` 为错误信息。

```
Response 200 (completed):
{
  "task_id": "...",
  "status": "completed",
  "result": {
    "overview": { "health_score": 82, "risk_level": "low", ... },
    "expert_scores": [ ... ],
    "risk_categories": [ ... ],
    "action_items": [ ... ],
    "conclusion": { ... },
    "dropped_findings": []
  },
  "error": null,
  ...
}

Response 404:   // task_id 不存在
{ "error": "task not found" }
```

---

### 3. 配置

#### `GET /api/v1/config`

返回当前生效的配置（UI 兼容的 `UiConfig` 结构，camelCase 字段）。

敏感字段永不回显真实值：已配置的 GitLab `apiToken` 与 LLM `apiKey` 一律以 `***` 掩码返回。`PUT /api/v1/config` 时：LLM key 传空串或 `***` 均表示「保持现有值」；GitLab token 传 `***` 表示保持，传空串表示显式清除。

#### `PUT /api/v1/config`

保存 UI 配置：重建 LLM provider 列表、更新并发上限，并同步 GitLab webhook 运行时配置（token / secret，无需重启）。

```
Request: UiConfig JSON（gitlab / llm / rules / advanced 四组字段）

Response 200:
{
  "status": "saved"
}
```

#### `POST /api/v1/config/test`

测试指定 LLM provider 配置的连通性（请求 `/models`，10s 超时）。

```
Request:
{
  "provider": "openai",
  "model": "gpt-4o",
  "api_key": "sk-...",
  "api_base": "https://api.openai.com/v1"
}

Response 200:
{
  "success": true,
  "latencyMs": 320,
  "error": null,
  "timestamp": "2026-07-18T02:00:00Z"
}
```

#### `POST /api/v1/config/models`

拉取指定 API base 下的可用模型列表（OpenAI 兼容 `/models` 接口）。

```
Request:
{
  "api_base": "https://api.openai.com/v1",
  "api_key": "sk-..."
}

Response 200:
{
  "models": ["gpt-4o", "..."]
}
```

#### `GET /api/v1/config/schema`

返回 JSON Schema（由 `schemars` 从 `AppConfig` struct 生成），Web UI 可据此动态渲染配置编辑表单。

#### `POST /api/v1/config/validate`

```
Request:
  "body": "toml 字符串..."

Response 200:
{
  "valid": true,
  "experts_count": 11
}

Response 422:
{
  "valid": false,
  "errors": ["unknown field 'foo'", "weight sum must be 100"]
}
```

---

### 4. 队列监控（Queue Monitor）

队列相关接口挂载于 `/api/v1/queue/`，供 Queue Monitor 页面使用。

#### `GET /api/v1/queue/stats`

返回队列实时统计。

```
Response 200:
{
  "active": 0,
  "queued": 0,
  "failed": 0,
  "totalDepth": 0,
  "maxConcurrent": 8,
  "queueCapacity": 16,
  "failedLast24h": 0,
  "totalLast24h": 0,
  "isPaused": false
}
```

`cancelled` 任务不计入 `failed` 或 `failedLast24h`。

#### `GET /api/v1/queue/tasks`

分页列出任务，支持按状态过滤。

```
Query:
  ?status=failed&page=1&per_page=50

status: running | queued | failed | completed | cancelled

Response 200:
{
  "items": [
    {
      "id": "550e8400-e29b-41d4-a716-446655440000",
      "mrTitle": "...",
      "project": "...",
      "repository": "...",
      "status": "failed",
      "progress": 85,
      "expertName": "Security",
      "elapsedMs": 12345,
      "createdAt": "2026-06-26T12:00:00Z",
      "startedAt": "2026-06-26T12:00:01Z",
      "errorMessage": "..."
    }
  ],
  "total": 1,
  "page": 1,
  "per_page": 50
}
```

Default `per_page`: 50, max `per_page`: 100.

#### `DELETE /api/v1/queue/tasks/{task_id}`

取消处于 `pending` 或 `running` 状态的任务。

```
Response 200:
{
  "status": "deleted"
}

Response 400:
{
  "error": "task not found or cannot be cancelled"
}
```

#### `POST /api/v1/queue/tasks/{task_id}/retry`

将失败任务重新加入队列。

```
Response 200:
{
  "status": "retried"
}

Response 400:
{
  "error": "task not found or not in failed state"
}
```

#### `POST /api/v1/queue/pause`

暂停队列：新任务保持 pending 但不会被启动。

```
Response 200:
{
  "status": "paused"
}
```

#### `POST /api/v1/queue/resume`

恢复队列。

```
Response 200:
{
  "status": "resumed"
}
```

#### `POST /api/v1/queue/max-concurrent`

设置最大并发任务数。

```
Request:
{
  "max_concurrent": 4
}

Response 200:
{
  "maxConcurrent": 4
}
```

所有队列接口在未初始化 task store 时返回 `503`：

```
Response 503:
{
  "error": "task store not initialized"
}
```

---

### 5. 系统

#### `GET /api/v1/system/experts`

```
Response 200:
{
  "experts": [
    {
      "id": "security",
      "name": "Staff Security Engineer",
      "category": "security",
      "icon": "Lock",
      "enabled": true,
      "weight": 15,
      "description": "Security Lead",
      "promptPreview": "You are the Security Lead...",
      "lastReviews": []
    }
  ]
}
```

VSCode Extension 可用此接口展示可选专家、让用户开关。

#### `PUT /api/v1/system/experts/{id}`

更新单个专家的启用状态与权重（`{id}` 为专家名 slug，如 `security`）。请求体字段都是可选的，只提交需要改的字段。

```
Request:
{
  "enabled": false,
  "weight": 20
}

Response 200: 更新后的专家对象（结构同 GET），额外带一个 `persisted` 字段：
{
  "id": "security",
  "enabled": false,
  "weight": 20,
  ...,
  "persisted": true
}
Response 404: { "error": "expert not found" }
Response 422: { "error": "invalid weight 200: an expert's weight must be between 0 and 100" }
Response 500: { "error": "failed to persist the expert change to the database: ..." }
```

请求体字段都可选，`{}` 表示「不改任何字段」，不会写入空的 override（专家的 `enabled`/`weight` 保持原值）。

`weight` 的合法范围是 **0–100**（与配置文件 `[review_experts.*]` 的 `weight` 一致），超出范围返回 `422` —— 与 `PUT /config` 对非法值的处理一致；之所以拒绝而不是静默截断，是因为 UI 的滑杆不可能产生越界值，能产生的只有手写客户端，静默存一个与请求不同的值会让接口的返回变成假话。

`persisted` 的语义（RENG-69）：

- `true` — 改动已写入配置数据库，重启后仍生效；
- `false` — 服务没有挂数据库（`REVIEW_DISABLE_DB=1`、嵌入式使用），改动只在内存里生效，重启即丢失。前端据此显示警告而不是成功提示；
- 写库失败时返回 `500`，调用方不得当作保存成功。

**顺序：先落库、再生效。** 写库（`await`）发生在改动运行态之前，所以 `500` 意味着这次请求什么都没改：运行中的 `app_config`、`GET /system/experts`、数据库都还是旧值（与 `PUT /config` 的「先内存后落库 + 失败留痕」不同，这里不需要回滚）。延迟不变——接口本来就要等写库完成才回响应，只是内存变更的时点后移；没有数据库时没有可等的事情，直接生效并返回 `persisted: false`。

改动生效后同时作用于 REST 提交的 review、webhook 触发的 review 与仓库扫描——这些路径各自重新解析配置文件，服务会把已持久化的 override 叠加到它们解析出的 `[review_experts]` 上（数据库覆盖配置文件，见 [configuration.md](configuration.md#experts-page-experts)）。

#### `GET /api/v1/system/version`

```
Response 200:
{
  "version": "0.9.0",
  "commit": "unknown",
  "features": ["cli", "python"]
}
```

#### `GET /api/v1/system/health`

返回集成与 LLM provider 的配置状态。

```
Response 200:
{
  "integrations": [
    { "service": "GitLab API", "type": "integration", "status": "offline", "latencyMs": 0, "message": "Not configured" }
  ],
  "llmProviders": [
    { "service": "openai gpt-4o", "type": "llm", "status": "success", "latencyMs": 0, "message": "Configured" }
  ],
  "overall": "success",
  "lastChecked": "2026-07-18T02:00:00Z"
}
```

`llmProviders`（0.10.18 起，RENG-36）：与 `GET /api/v1/llm/providers`、Dashboard 的 `health` 段读同一份探测缓存，`status` 取最近一次真实探测结果（`success` / `error` / `offline`），`message` 为具体错误文本（如 `HTTP 401 Unauthorized`），`overall` 同样按探测结果得出（全正常 `success` / 部分 `warning` / 全失败 `error` / 无 provider `offline`）。本端点不返回探测延迟（`latencyMs` 恒为 0，按 RENG-32 的 Dashboard 规则；卡片接口才带真实耗时）。注意 `llmConfigured` 仍是**配置**判据（有非空 `api_base` 即可），与健康状态无关。

顶层 `GET /health`（及 `/health/ready`）保留，用于存活检查，返回简单状态（见 §7 认证策略）。

#### `GET /api/v1/system/upgrade/check`

检查是否有新版本可用。结果在服务端缓存 **1 小时**（GitHub 未认证 API 限流 60 次/小时/IP），缓存命中时不发起网络请求；过期或未缓存时触发一次实时检查，并在无升级任务进行的情况下短暂将任务状态置为 `checking`。

```
Response 200:
{
  "currentVersion": "0.8.2",
  "latestVersion": "0.9.0",
  "updateAvailable": true,
  "installMethod": "binary",             // binary | brew | docker | cargo | unknown
  "platformAssetAvailable": true,
  "releaseUrl": "https://github.com/Liewzheng/ReviewEngine/releases/tag/v0.9.0",
  "upgradeHint": "reng upgrade",         // 按安装方式给出的升级命令（与 CLI 提示一致）
  "cachedAt": "2026-08-03T10:00:00Z"     // RFC3339；从未缓存过则为空字符串
}

Response 502:
{ "error": "check failed: ..." }
```

- `installMethod` 取值：`binary`（直接部署，可自动升级）/ `brew` / `docker` / `cargo` / `unknown`。
- `upgradeHint` 对应各安装方式的升级命令：`binary` → `reng upgrade`；`brew` → `brew upgrade review-engine`；`cargo` → `cargo install review-engine --locked --features cli`；`docker` → `Web UI 或 reng upgrade 自动升级（容器将自动重启）`；`unknown` → 官方 `install.sh` 手动升级。

#### `POST /api/v1/system/upgrade`

按检测到的安装方式执行升级。`binary`（直接部署）与 `docker`（容器内）都会启动后台任务完成「下载 → SHA256 校验 → 解压 → 替换前冒烟测试 → 备份 → 原子替换 → 替换后复验」，失败即回滚并保留备份。同一时间只允许一个任务进行（single-flight），进行中并发请求返回 `409`。**是否重启取决于安装方式**：`binary` 直接部署下运行中的进程不会被重启，任务置为 `done` 表示磁盘上的二进制已替换，需手动重启服务生效；`docker` 容器下服务会**替换二进制与前端 dist 后主动 exit**，由 compose 的 `restart: unless-stopped` 自动拉起新版本（无需重建镜像）。

```
Response 202 (binary / docker，任务已启动):
{
  "status": "started",
  "targetVersion": "0.9.0"
}

Response 400 (brew / cargo / unknown，返回手动升级提示):
{ "error": "检测到 Homebrew 安装，请手动执行升级命令", "upgradeHint": "brew upgrade review-engine" }
{ "error": "检测到 cargo 安装，请手动执行升级命令", "upgradeHint": "cargo install review-engine --locked --features cli" }
{ "error": "无法识别安装方式，请使用官方 install.sh 手动升级", "upgradeHint": "使用官方 install.sh 手动升级" }

Response 400 (当前平台无对应 release 资产 / 缺少 sha256 校验资产，无法自动升级):
{ "error": "no release asset for this platform" }
{ "error": "release has no checksum asset for this platform" }

Response 409 (已有升级任务进行中):
{ "error": "升级任务已在进行中，请稍后再试" }

Response 502 (最新版本检查失败):
{ "error": "check failed: ..." }
```

#### `GET /api/v1/system/upgrade/status`

返回当前升级任务的 8 态状态机快照：

| state | 含义 |
|-------|------|
| `idle` | 无任务（默认） |
| `checking` | 检查最新版本 |
| `downloading` | 下载 release 资产 |
| `verifying` | 校验 SHA256 |
| `installing` | 解压并替换二进制 |
| `done` | 完成 — binary 直接部署需手动重启生效；docker 容器随即自动重启 |
| `failed` | 失败（`message` 含原因） |
| `notSupported` | 保留状态：当前安装/平台不支持自升级（docker 已支持容器内自升级；平台无 release 资产时在 `POST` 阶段即返回 400，一般不进入此态） |

`checking` / `downloading` / `verifying` / `installing` 为「进行中」状态（single-flight 门控：此时并发 `POST /api/v1/system/upgrade` 返回 `409`）。

```
Response 200:
{
  "state": "downloading",
  "message": "正在下载 release 资产",
  "currentVersion": "0.8.2",
  "targetVersion": "0.9.0"      // 升级任务进行中有值；空闲/失败为 null（binary 与 docker 均如此）
}
```

以上三个端点都在 `/api/v1` 鉴权层内（见 §7 认证策略）。

---

### 6. 实时推送（SSE）

#### `GET /api/v1/events`

```
data: {"task_id":"...","status":"completed","event":"review.completed"}

data: {"task_id":"...","status":"running","event":"review.started"}

data: {"task_id":"...","status":"cancelled","event":"review.cancelled"}
```

Web UI 和 Desktop App 通过 `EventSource` 监听，无需轮询。

#### `GET /api/v1/logs`

日志实时流（SSE）。每条 `data` 为一条日志 entry 的 JSON，15s 心跳保活。日志收集器未初始化时返回 `503`。

#### `GET /api/v1/logs/download`

批量下载最近日志（最多 1000 条），`Content-Type: application/x-ndjson`，每行一条 JSON。

---

### 7. LLM Providers 管理

多 provider 的增删改查与连通性测试。Provider id 格式为 `{provider}-{index}`（如 `openai-0`）。

#### `GET /api/v1/llm/providers`

```
Response 200:
{
  "usageWindowDays": 7,
  "usageSince": "2026-09-08T02:00:00Z",
  "usageAvailable": true,
  "usageTotal": 88,
  "latencyWindowDays": 7,
  "latencySince": "2026-09-08T02:00:00Z",
  "latencyAvailable": true,
  "items": [
    {
      "id": "openai-0",
      "name": "openai",
      "logo": "OpenAI",
      "status": "healthy",
      "configured": true,
      "apiBaseUrl": "https://api.openai.com/v1",
      "defaultModel": "gpt-4o",
      "maxTokens": 4096,
      "temperature": 0.3,
      "position": 0,
      "chainPosition": 1,
      "isPrimary": true,
      "lastProbeLatencyMs": 320,
      "requestCount": 12,
      "usageShare": 0.75,
      "successRate": 0.9167,
      "lastUsedAt": "2026-09-14T08:30:00+00:00",
      "avgLatencyMs": 812,
      "latencySampleCount": 46,
      "latencyFailureCount": 3,
      "latencyLastSampleAt": "2026-09-15T01:58:00+00:00",
      "latencySparkline": [780, null, 812, 940, null, 760],
      "lastChecked": "2026-09-15T02:00:00Z"
    }
  ]
}
```

API key 永远不会在响应中返回。

`status` / `lastProbeLatencyMs` / `lastChecked` 来自**真实探测**（0.10.18 起，RENG-36；字段名 0.10.23 起由 `latencyMs` 改为 `lastProbeLatencyMs`，见下）：服务端对每个已配置 provider 发一次 `GET {api_base}/models`（与 `POST …/{id}/test`、CLI `reng config provider test` 同一探测路径），并把结果按 provider 缓存 60s。取值：

- `healthy` —— 最近一次探测成功（`message: Configured`），`lastProbeLatencyMs` 是该次探测的往返耗时，`lastChecked` 是探测时刻；
- `error` —— 最近一次探测失败（key 被改坏 / 被吊销、地址不可达、401/403 等），`message` 是具体错误；
- `offline` —— 没有存储 key，**不做探测**，`lastProbeLatencyMs` 为 0；`lastChecked` 为 `null`（没有任何一次探测发生过，不再回填当前时间）。

改 key、改 `apiBase`、改 provider 名、删除 provider（`PUT /api/v1/config` 的 `llm` 段，或本节的 `POST` / `PUT` / `DELETE /providers`）都会**丢弃该 provider 缓存的健康状态**，下一次读取重新探测后才给出状态 —— 因此「在 WebUI 改坏 key、不重启服务」不会再显示成 `healthy`。失效粒度是**按 provider**（缓存键是 `provider + model + api_base + api_key` 的 SHA-256 指纹）：只动一个 provider 时，其他 provider 的状态与徽标不受影响，也不会被连带重新探测。缓存未命中时读取会等待该次探测（最长即探测自身的 10s 超时）；只是超过 TTL 的条目会立即返回并**在后台**刷新一次，所以正常轮询不会因为探测而变慢。

`POST /api/v1/llm/providers/{id}/test` 的响应仍然叫 `latencyMs`：那是**用户刚发起的那一次**手工测试自己的往返耗时（RENG-54 的会话内结果行），与列表里的探测缓存是两个不同的测量，不共用字段名。

0.10.11 起（RENG-55）每个 provider 额外返回链序信息：`position` 为它在**存储列表**中的下标（0 起，与 `llm_providers.raw.position` 及 UI 卡片顺序一致，不受“首选”选择影响），`chainPosition` 为它在**运行时链**中的 1 起名次（首选 provider 为 1，其后按存储顺序排列），`isPrimary` 标识链首（即评审实际首先使用的 provider）。运行时链的规则见 [configuration.md](configuration.md#chain-order-and-the-primary-provider)。

0.10.21 起（RENG-56）每个 provider 额外返回**真实使用统计**，数据源是评审记录本身（`reviews.llm_summary`，RENG-38 起每次评审写入的 `[{provider, model}]` 快照）而不是任何估算值：

- 窗口：`usageWindowDays`（当前恒为 7）与 `usageSince`（滚动窗口起点，含端点）随列表一起返回 —— UI 用它标注「过去 7 天」，不自行假设窗口。
- `usageTotal`：窗口内**全部**已记录使用数（所有 provider 名，含已不再配置的），即每个 `usageShare` 的分母。因此它可以大于各卡片 `requestCount` 之和：卡片只统计当前配置里的 provider。`null` 表示读不到历史。
- `requestCount`：该 provider 在窗口内被记录到的**评审数**（评审级粒度：一次评审无论用几个模型，都只给该 provider 记一次）。可直接用 `GET /api/v1/reviews` 的 `llmSummary` 逐条核对。
- `usageShare`：该 provider 占窗口内**全部**已记录使用（含已不再配置的 provider 名）的比例，0–1。
- `successRate`：使用过该 provider 且已终态的评审中 `completed / (completed + failed)`，0–1。
- `lastUsedAt`：窗口内最近一次使用该 provider 的时刻。

**不知道就是 `null`，绝不填 0**：窗口内没有任何记录的 provider，`requestCount` 是实测的 `0`，而 `usageShare` / `successRate` / `lastUsedAt` 为 `null`（没有分母 / 没有终态 / 从未使用）。`usageAvailable` 为 `false`（`REVIEW_DISABLE_DB=1`、聚合查询失败）时四项全为 `null`。`successRate` 的固有局限：`llm_summary` 只在评审完成写回时落库，因此「还没产出任何报告就失败的评审」不带快照、也无法归因到某个 provider —— 该比率是「用过它并且跑完的评审里有多少成功」，是 provider 自身调用成功率的**上界**；逐次调用的成功率/延迟见下面 RENG-57 的 `llm_call_samples`。

`usagePercent`（限额容量）已从响应中**删除**：不存在限额概念，原先恒为 `0` 的占位字段与硬编码的 `errorRate` 一并移除，避免页面展示假数据。

成本：每次读取一次聚合查询，走 `reviews(created_at)` 索引的范围扫描，代价与窗口内评审数成正比（窗口外与 `llm_summary IS NULL` 的行在同一次扫描中被过滤），JSON 快照在 Rust 侧解析（SQLite / PostgreSQL 两端无需 JSON 方言分叉）。

0.10.23 起（RENG-57）额外返回**逐次调用的真实延迟统计**，数据源是评审路径每次 LLM 调用落库的采样表 `llm_call_samples`（迁移 `0004_llm_call_samples.sql`）。此前页面的「平均延迟」只有探测的瞬时值可用（RENG-53 的困惑点正是这两种测量被混为一谈）：

- 窗口：`latencyWindowDays`（当前恒为 7）与 `latencySince`（滚动窗口起点，含端点）**独立于 usage 窗口单独返回**，客户端不假设两者一致（当前实现两者同为 7 天）。
- `avgLatencyMs`：窗口内该 provider **成功调用**的平均往返耗时（整数毫秒）。失败调用**不计入**均值（一次 401 可能 5ms 返回、一次超时可能 120s，混入会让均值反映错误分布而非 provider 速度），失败次数单独给出。
- `latencySampleCount` / `latencyFailureCount`：窗口内的成功 / 失败调用次数（采样表的行数口径，逐次尝试计数：重试与 fallback 的每一次失败尝试都各占一行）。`latencySampleCount` 是均值的分母。
- `latencyLastSampleAt`：窗口内最近一次调用（成功或失败）的时刻。
- `latencySparkline`：窗口按 6 小时切成 28 桶、每桶成功调用的平均耗时（整数毫秒），最旧桶在前；桶内无调用为 `null`（折线断开，不画假值）。**没有采样就是 `null`**（没有可画的序列，也不会画一条零线）。
- `lastProbeLatencyMs`：探测的瞬时往返耗时（RENG-36），与上面的历史均值是**不同字段**，页面也分开显示（卡片指标行显示历史均值，探测值显示在「Last checked」一行的 `Probe {n} ms`）。
- `latencyAvailable`：`false` 表示采样表读不到（无 DB / 查询失败），此时六个延迟字段全为 `null`。

**不知道就是 `null`**：窗口内没有成功调用的 provider，`avgLatencyMs` 为 `null`（页面显示 `—`），`latencySampleCount` / `latencyFailureCount` 是实测的 `0`（实测计数可以是 0，均值不能）。

写入路径（best-effort，绝不影响评审）：`LLMClient` 每次调用尝试结束后把一行交给 `StoreLlmCallSink`，它写 `llm_call_samples` 并在**每个 sink 的第一次写入**时顺带做一次保留期清理（删除 30 天前的行，`src/store/llm_samples.rs` 的 `RETENTION_DAYS = 30`）。写失败只记 WARN（与 `llm_summary` 写穿一致）；无 DB 时不挂 sink，什么都不写。Repo 扫描类评审（`/api/v1/repo/*`）不在覆盖范围内：它的报告不带 provider 归因（`llm_provider: None`），RENG-56 的 usage 统计同样看不到它。

成本：每次读取一次 `llm_call_samples(created_at, provider)` 索引的窗口范围扫描，行数按窗口内实际调用数计（典型规模见 `docs/configuration.md`），在 Rust 侧折叠为每 provider 的均值与分桶（同 RENG-56 的理由：不做 SQLite / PostgreSQL 的日期分桶方言分叉）。

#### `POST /api/v1/llm/providers`

新增 provider。必填 `provider` 与 `api_key`；`model`（别名 `defaultModel`）、`api_base`（别名 `apiBaseUrl`）、`max_tokens`、`temperature` 可选。

```
Response 201:
{
  "id": "openai-1",
  "provider": "openai",
  "model": "gpt-4o",
  "configured": true
}

Response 400: { "error": "provider name is required" } / { "error": "api_key is required" }
```

#### `PUT /api/v1/llm/providers/{id}`

更新 provider（非空字段才会覆盖；`max_tokens` / `temperature` 总是更新）。

```
Response 200: { "status": "updated", "id": "openai-0", "provider": "openai", "model": "gpt-4o" }
Response 404: { "error": "Provider not found" }
```

#### `DELETE /api/v1/llm/providers/{id}`

```
Response 200: { "status": "deleted", "id": "openai-0" }
Response 404: { "error": "Provider not found" }
```

#### `POST /api/v1/llm/providers/{id}/test`

测试该 provider 的连通性（请求 `/models`，10s 超时）。

```
Response 200:
{
  "success": true,
  "latencyMs": 320,
  "error": null,
  "timestamp": "2026-07-18T02:00:00Z"
}
```

0.10.18 起（RENG-36）这次探测的结果同样写入该 provider 的健康缓存，所以紧接着的 `GET /llm/providers` 会直接给出「刚测过」的状态（与卡片上的 Test Connection 结论一致），而不是再探一次。

---

### 8. Dashboard 聚合

#### `GET /api/v1/dashboard`

聚合返回 Dashboard 页面所需的 KPI、24h 趋势、14 天按日趋势、系统健康与最近 reviews。

**数据源（0.10.8 / RENG-32）**：KPI、24h 趋势、recentReviews 一律来自持久化 `reviews` 表（0.10.0 SQLite/PG 存储）——内存任务队列的 reaper 会在完成 30 分钟后删除条目，曾导致「库里 100+ 条、Dashboard 全零」。`activeQueue` 是唯一保留内存数据源的字段（活跃 pending+running 是实时队列状态，DB 反而是错源）。`REVIEW_DISABLE_DB=1` / 无 DB 时整体回退到内存任务存储（与 `/reviews` 的 DB 优先、内存兜底策略一致）；两者皆无则返回默认值。历史读取失败返回 `500`（与 `/reviews` 相同），而不是编造零值。

**窗口语义**：全部按服务器本地时区计算（容器内请设置 `TZ` 固定时区，否则用镜像默认的 UTC）。「本周」= 当前 ISO 周（周一 00:00 起）；「今日/昨日」= 本地自然日。

**KPI 语义与空数据表示**：

- `reviewsThisWeek`：本周创建的评审数（不限状态）。
- `reviewsTrend`：本周 vs 上周的相对百分比变化（如 `+25.0`）；上周无数据时为 `null`。
- `activeQueue`：内存队列中 pending + running 的实时数量。
- `successRate`：本周 `completed/(completed+failed)`（%），窗口与卡片「本周」标签一致；本周无终态评审时为 `null`。
- `successTrend`：今日成功率 vs 昨日的**百分比点位差**（如 `+20.0`）；任一昨日/今日无终态评审时为 `null`。
- `avgDurationMs`：本周已完成评审的平均耗时；本周无已完成评审时为 `null`。
- `durationTrend`：本周平均耗时 vs 上周的相对百分比变化；任一窗口无数据时为 `null`。

所有 `null` 在前端渲染为「—」，绝不显示编造的 `0.0%`。

**成本**：前端每 60s 轮询。计数走 SQL `COUNT(*)`（只取 total，不物化行）；平均耗时 / 趋势分桶 / recentReviews 才物化行，各窗口最多取最新 500 条（`created_at` 索引范围扫描）。24h 与 14 天按日两条趋势序列**共用同一次**有界窗口行拉取（13 天前的本地零点起，覆盖 24h 窗口），轮询不额外增加查询次数。

```
Response 200:
{
  "kpis": {
    "reviewsThisWeek": 12,
    "reviewsTrend": 25.0,          // 或 null（上周无数据）
    "activeQueue": 1,
    "successRate": 91.7,           // 或 null（本周无终态评审）
    "successTrend": 5.0,           // 或 null（昨日/今日无终态评审）
    "avgDurationMs": 28400,        // 或 null（本周无已完成评审）
    "durationTrend": -12.5         // 或 null（对比窗口无数据）
  },
  "trend": [ { "time": 1789948800, "value": 2 } ],
  "trendDaily": [ { "time": 1789056000, "value": 5 } ],
  "health": { "integrations": [], "llmProviders": [], "overall": "success", "lastChecked": "..." },
  "recentReviews": [
    {
      "id": "550e8400-...",
      "mrTitle": "Fix login",
      "project": "owner/repo",
      "author": { "name": "alice", "avatarUrl": null },
      "status": "completed",       // pending | running | completed | failed | cancelled
      "durationMs": 28400,
      "createdAt": "2026-07-18T02:00:00Z"
    }
  ]
}
```

`trend`：最近 24 小时的 24 个小时桶（按 `created_at` 归桶，保留旧版滚动小时窗口形状），卡片合计 = 窗口内总和。

`trendDaily`（0.10.9 / RENG-50）：最近 14 个**自然日**（含今日，今日为最后一个不完整桶）的按日分桶，恰好 14 个点，最旧 → 最新（与 `trend` 排序约定一致）。`time` = 当日本地 00:00 的 unix 时间戳（与 KPI「今日/昨日」窗口同一 `day_start` 本地时区锚点，按日历日归桶——跨 DST  transition 的 23h/25h 日也是整日历日）；`value` = `created_at` 落入 `[当日 00:00, 次日 00:00)` 的评审数（不限状态）。与 `trend` 共用同一次有界行拉取（窗口内最新 ≤500 行），极重负载下同样退化为「窗口内最新 500 行」。无存储的默认响应同样给出 14 个零值点（时间戳锚点不变）。

`recentReviews`：最新 5 条（`created_at` DESC），不再按状态过滤（pending / running / completed / failed / cancelled 都会出现），`status` 使用与 `/reviews` 一致的真实状态词汇（不再输出 `"success"`）。

`health.integrations`（0.10.8 起）：按**实际 git 集成配置**检测，两条配置通道任一满足即报 `success`——`git_platforms` 表（即 `PUT /api/v1/config` 的 Git 平台列表）中存在任一 `type=gitlab` / `type=github` 平台，**或**启动时经 env/CLI 配置了凭据（`GITLAB_TOKEN` / `--gitlab-token`、`GITHUB_TOKEN` / `--github-token`；该通道直接接入 webhook / MR 拉取客户端，不经过 `git_platforms`）；不再通过 LLM provider 名称猜测。`latencyMs` 字段已移除（原恒为 0 的占位值；真实连通性/延迟探测用 `POST /api/v1/llm/providers/{id}/test`）。

`health.llmProviders`（0.10.18 起，RENG-36）：与 `GET /api/v1/llm/providers` 共用同一份健康缓存（`AppState::llm_health`），因此两页不会互相矛盾。每行 `status` 取该 provider 最近一次真实探测的结果（`success` / `error` / `offline`，`offline` = 未配置 key、不探测），`message` 为 `Configured` / `Missing API key` / 具体错误文本（如 `HTTP 401 Unauthorized`）。`overall` 同样按探测结果得出：无 provider 为 `offline`；**全部**正常为 `success`；部分正常为 `warning`（含「有一个 provider 未配置 key」的情形）；一个都不正常为 `error`——不再只看「有没有配 key」。改配置（`PUT /api/v1/config` 的 `llm` 段或 provider 增删改）会丢弃受影响 provider 的缓存并在下次读取时重新探测；读取路径上未被缓存的 provider 会等待该次探测（最长 10s），仅超 TTL 的条目在后台刷新。

---

### 9. Finding 反馈闭环

用户对单条 finding 打「有用 / 误报」标记，服务端按稳定 fingerprint 归并统计命中率与误报率，为后续 prompt 校准和降误报提供数据基础（设计见 `docs/professional_team_design.md` §6.3 / §8.9）。

**fingerprint**：对 `(file, line, title, category)` 做 SHA-256（字段间以 `0x1f` 分隔），取前 16 个 hex 字符。同一 finding 在多次评审中 fingerprint 不变。

**存储**：JSON 数组，默认落在状态目录（默认 `~/.config/review-engine/feedback.json`，`serve --data-dir` 会移动整个目录），可用环境变量 `REVIEW_FEEDBACK_PATH` 覆盖；写入为原子写（tmp + rename）。

**生效**：被标为误报（`false_positive`）的 finding 将在后续评审中被自动过滤——按 fingerprint 匹配，在验证 pass 之后、lead consolidation 之前移除，并计入 `dropped_findings`（reason 为 "marked false positive by user feedback"）；可用 `[report] feedback_filtering = false` 关闭（feedback 文件缺失或读失败时静默跳过，不影响评审）。同一 fingerprint 存在多次反馈时，以 `created_at` 最新的裁决为准——误标误报后再补一条 `useful` 即可解除过滤。

#### `POST /api/v1/feedback`

记录一条反馈。`verdict` 必填，取值 `"useful"` | `"false_positive"`。finding 有两种定位方式，二选一：

- 直接给 `finding_fingerprint`（非空字符串）；
- 或给 `file` + `title` + `category`（`line` 可选），由服务端计算 fingerprint（此时 `category` 会随记录保存，用于分类统计）。

```
Request（便捷形式）:
{
  "file": "src/main.rs",
  "line": 42,
  "title": "SQL injection risk",
  "category": "security",
  "verdict": "false_positive",
  "comment": "input is sanitised upstream"   // 可选
}

Request（fingerprint 形式）:
{
  "finding_fingerprint": "9f2c1ab7e04d3a55",
  "verdict": "useful"
}

Response 200:
{
  "finding_fingerprint": "9f2c1ab7e04d3a55",
  "verdict": "false_positive",
  "comment": "input is sanitised upstream",
  "category": "security",
  "created_at": "2026-07-18T03:30:00Z"
}
```

错误：`400` body 非法 / 缺 `verdict` / 两种定位方式都不完整；`503` feedback store 未初始化。

#### `GET /api/v1/feedback/stats`

聚合统计。`false_positive_rate = false_positive / total`（无数据时为 `0.0`）。按 fingerprint 形式提交、未带 `category` 的记录归入 `"unknown"` 桶。

```
Response 200:
{
  "total": 4,
  "useful": 2,
  "false_positive": 2,
  "false_positive_rate": 0.5,
  "by_category": {
    "security": { "total": 2, "useful": 1, "false_positive": 1, "false_positive_rate": 0.5 },
    "quality":  { "total": 1, "useful": 1, "false_positive": 0, "false_positive_rate": 0.0 },
    "unknown":  { "total": 1, "useful": 0, "false_positive": 1, "false_positive_rate": 1.0 }
  }
}
```

---

## 数据流

```
Client                    Server                        Core
  │                         │                            │
  │  POST /api/v1/reviews   │                            │
  │ ──────────────────────► │                            │
  │  202 { task_id }        │  生成 UUID + 入 TaskStore   │
  │ ◄────────────────────── │                            │
  │                         │                            │
  │                         │  tokio::spawn               │
  │                         │ ─────────────────────────► │
  │                         │   orchestrator::run_experts │
  │  GET /api/v1/reviews/   │   ◄──────────────────────── │
  │    {task_id}            │  存结果到 TaskStore         │
  │ ──────────────────────► │                            │
  │  200 { status, result } │                            │
  │ ◄────────────────────── │                            │
```

---

## 新增文件清单

| 文件 | 职责 | 行数 |
|------|------|------|
| `src/server/api/mod.rs` | 路由注册 + CORS 配置（`tower-http`） | 20 |
| `src/server/api/review.rs` | review CRUD endpoints | 220 |
| `src/server/api/config.rs` | config + schema + validate | 70 |
| `src/server/api/system.rs` | experts + version | 40 |
| `src/server/api/queue.rs` | queue stats, tasks, pause/resume/retry | 227 |
| `src/server/api/types.rs` | TaskStatus, ReviewRequest 等 | 70 |
| `src/server/task_queue.rs` | 内存 TaskStore（`Arc<RwLock<HashMap>>`） | 100 |
| `src/server/auth.rs` | Bearer token 验证中间件 | ~30 |
| **合计** | | **~777 行** |

---

## 前端接入路径

| 前端 | 接入方式 | 关键依赖 |
|------|---------|---------|
| **Web UI** | 用户启动 `reng serve`，前端 AJAX → `localhost:8080` | CORS + 无 auth（localhost） |
| **Desktop App** | 同机或内嵌启动 server，HTTP 通信 | 同上 + token auth 可选 |
| **VSCode Extension** | 方案 A（推荐）：extension 激活时 `reng serve --port 9123` 启动后台进程，通过 `localhost:9123/api/v1/*` 通信，extension 退出时 kill | 同 Web UI |
| | 方案 B（简单）：extension 每次调 CLI 子进程 `--format json` + 解析 stdout | 无 server 依赖，但每次冷启动 LLM |

---

## 实施顺序

| 阶段 | 内容 | 前置依赖 |
|------|------|---------|
| 1 | `types.rs` + `task_queue.rs` — 异步任务基础设施 | 无 |
| 2 | `api/mod.rs` + `api/review.rs` — 路由 + review 核心 | 阶段 1 |
| 3 | `api/system.rs` + `api/config.rs` — 补充端点 | 无 |
| 4 | `auth.rs` + 绑定地址校验 — 认证中间件 | 无 |
| 5 | SSE 实时推送 — review 完成通知 | 阶段 1 |
| 6 | `api/config/schema` — JSON Schema 端点 | 需 `schemars` 库 |

---

## 7. 认证策略

### 原则：按网络边界自动决定安全等级

| 监听地址 | 安全等级 | 认证要求 | 使用场景 |
|---------|---------|---------|---------|
| `127.0.0.1`（默认） | 高 — 内核隔离 | **不需要** | 本地开发、本机 VSCode Extension |
| `0.0.0.0` 或指定 IP | 低 — 网络可达 | **强制 API Token** | 局域网共享 / Docker / 公网 |

监听 `127.0.0.1` 时系统内核阻止外部 TCP 连接，无需额外认证。
监听 `0.0.0.0` 时 server **拒绝启动**直到用户提供 `--api-token`，防止意外暴露。

### Token 配置方式

按优先级从高到低：

| 方式 | 示例 | 适用场景 |
|------|------|---------|
| CLI 参数 | `--api-token xxxxx` | 临时启动、测试 |
| 环境变量 | `REVIEW_API_TOKEN=xxxxx` | Docker、CI/CD |
| 配置文件 | `[server] api_token = "xxxxx"` | 持久化部署 |

### 辅助命令

```bash
# 生成 32 字节随机 Token
reng generate-token
# → review_a1b2c3d4e5f6g7h8i9j0k1l2m3n4o5p

# 安全启动方式
reng serve --bind 0.0.0.0 --api-token $(reng generate-token)
```

### 哪些路由需要认证

| 路由 | `127.0.0.1` | `0.0.0.0` | 原因 |
|------|------------|-----------|------|
| `GET /health` | 不认证 | 不认证 | 存活检查，无敏感信息 |
| `POST /api/v1/reviews` | 不认证 | **认证** | 消耗 LLM token，有成本风险 |
| `POST /api/v1/reviews/:id/rerun` | 不认证 | **认证** | 重新消耗 LLM token |
| `GET /api/v1/reviews` | 不认证 | **认证** | 可能泄漏代码 diff |
| `GET /api/v1/reviews/:id` | 不认证 | **认证** | 同上 |
| `GET /api/v1/config` | 不认证 | **认证** | 可能泄漏敏感配置 |
| `POST /api/v1/config/validate` | 不认证 | **认证** | 位于 `/api/v1` 鉴权层内 |
| `GET /api/v1/system/version` | 不认证 | **认证** | 位于 `/api/v1` 鉴权层内 |
| `GET /api/v1/system/experts` | 不认证 | **认证** | 位于 `/api/v1` 鉴权层内 |
| `GET /api/v1/system/upgrade/check` | 不认证 | **认证** | 版本与 release 信息 |
| `POST /api/v1/system/upgrade` | 不认证 | **认证** | 修改服务端二进制（自升级） |
| `GET /api/v1/system/upgrade/status` | 不认证 | **认证** | 升级任务状态 |
| `GET /api/v1/queue/stats` | 不认证 | **认证** | 位于 `/api/v1` 鉴权层内 |
| `GET /api/v1/queue/tasks` | 不认证 | **认证** | 可能包含 MR 标题与仓库信息 |
| `DELETE /api/v1/queue/tasks/:id` | 不认证 | **认证** | 控制任务状态 |
| `POST /api/v1/queue/tasks/:id/retry` | 不认证 | **认证** | 重新消耗 LLM token |
| `POST /api/v1/queue/pause` | 不认证 | **认证** | 控制队列状态 |
| `POST /api/v1/queue/resume` | 不认证 | **认证** | 控制队列状态 |
| `POST /api/v1/queue/max-concurrent` | 不认证 | **认证** | 修改并发配置 |

> 除 `GET /health` 外，所有 `/api/v1/*` 路由都挂在同一个鉴权中间件之后：配置了
> API token（非 loopback 绑定必填）时一律要求 `Authorization: Bearer` /
> `X-API-Key`，未带或错误返回 `401` + `{"error":"unauthorized"}`。

### 请求方式

支持两种方式传递 Token（客户端任选其一）：

```
# Bearer Token（标准 HTTP Auth）
Authorization: Bearer review_a1b2c3d4e5f6g7h8i9j0k1l2m3n4o5p

# X-API-Key Header
X-API-Key: review_a1b2c3d4e5f6g7h8i9j0k1l2m3n4o5p
```

### 轮换 API Token（`PUT /api/v1/system/token`）

设置或轮换 API Token：服务端持久化其摘要（auth.toml）并热切换生效，无需重启。

轮换仍需认证，但可接受以下任一凭证（避免 token 失效时无法自救的死锁）：

1. **当前有效 token** —— `Authorization: Bearer` / `X-API-Key`（常规路径）；
2. **Bootstrap Key** —— `X-Bootstrap-Key: <key>`（`REVIEW_BOOTSTRAP_KEY` /
   `--bootstrap-key`）。当当前 token 丢失或失效（例如浏览器 localStorage 里是旧值）
   时，用 Bootstrap Key 即可轮换到新 token —— 这是推荐的自救路径；
3. **env/CLI 显式 token** —— `REVIEW_API_TOKEN` / `--api-token` 指定的 token
   始终可作为轮换凭证（env 优先级覆盖），即使运行期已被 UI 轮换覆盖。

> 安全模型不变：轮换仍要求某种有效凭证；Bootstrap Key 只放行轮换端点，**不会**
> 解锁其他 `/api/v1/*` 端点（普通端点仍只认当前有效 token）。

### 与 GitLab webhook 的关系

已有独立的 `X-Gitlab-Token` 校验（`src/server/gitlab.rs`），三者不冲突：
- GitLab webhook → `X-Gitlab-Token` header（硬编码 webhook secret）
- API 请求 → `Authorization: Bearer` / `X-API-Key`（用户配置的 API token）
- `POST /api/v1/reviews`（`gitlab_mr`）→ `X-Gitlab-Token` header（GitLab 上游 API 凭证，见 §1 凭证传输要求；与 webhook secret 同名不同义，按路由区分）

```toml
[dependencies]
schemars = "0.8"        # 从 Rust struct 生成 JSON Schema
tower-http = { version = "0.6", features = ["cors"] }  # 已存在，需加 feature
uuid = { version = "1", features = ["v4"] }             # 已存在
serde = { version = "1", features = ["derive"] }        # 已存在
serde_json = "1"                                        # 已存在
```

现有 `Cargo.toml` 中 `tower-http` 和 `uuid` 已存在，只需确认 feature 开启。
