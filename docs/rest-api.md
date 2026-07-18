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
| `source.token` | `string` | GitLab token（仅 `gitlab_mr`） |
| `source.path` | `string` | 本地仓库路径（仅 `local_repo`） |
| `source.base` | `string` | base ref（仅 `local_repo`，默认 `main`） |
| `source.head` | `string` | head ref（仅 `local_repo`） |
| `source.diff` | `string` | 原始 diff 文本（仅 `static_diff`） |
| `config` | `string` | 可选 TOML 配置，覆盖默认 |
| `llm_configs` | `LLMConfig[]` | 可选 LLM 配置覆盖 |
| `webhook` | `string` | 可选回调 URL，完成后 POST 结果 |

#### `POST /api/v1/reviews`

```
Request:
{
  "source": {
    "type": "gitlab_mr",
    "url": "https://gitlab.com/owner/repo/-/merge_requests/23",
    "token": "glpat-xxx"
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

#### `GET /api/v1/reviews/:task_id`

```
Response 200:
{
  "task_id": "550e8400-e29b-41d4-a716-446655440000",
  "status": "completed",           // pending | running | completed | failed
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
  "error": null
}
```

#### `GET /api/v1/reviews`

分页列出历史 reviews。

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

#### `DELETE /api/v1/reviews/:task_id`

取消 `pending` 或 `running` 状态的 task。

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

#### `GET /api/v1/queue/tasks`

分页列出任务，支持按状态过滤。

```
Query:
  ?status=failed&page=1&per_page=50

status: running | queued | failed | completed

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

更新单个专家的启用状态与权重（`{id}` 为专家名 slug，如 `security`）。

```
Request:
{
  "enabled": false,
  "weight": 20
}

Response 200: 更新后的专家对象（结构同 GET）
Response 404: { "error": "expert not found" }
```

#### `GET /api/v1/system/version`

```
Response 200:
{
  "version": "0.7.9",
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

顶层 `GET /health`（及 `/health/ready`）保留，用于存活检查，返回简单状态（见 §7 认证策略）。

---

### 6. 实时推送（SSE）

#### `GET /api/v1/events`

```
data: {"task_id":"...","status":"completed","event":"review.completed"}

data: {"task_id":"...","status":"running","event":"review.started"}
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
      "latencyMs": 0,
      "errorRate": 0.0,
      "requestCount": 0,
      "usagePercent": 0,
      "sparkline": [],
      "lastChecked": "2026-07-18T02:00:00Z"
    }
  ]
}
```

API key 永远不会在响应中返回。

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

---

### 8. Dashboard 聚合

#### `GET /api/v1/dashboard`

聚合返回 Dashboard 页面所需的 KPI、24h 趋势、系统健康与最近 reviews。task store 未初始化时返回全零默认值。

```
Response 200:
{
  "kpis": {
    "reviewsThisWeek": 12,
    "reviewsTrend": 0.0,
    "activeQueue": 1,
    "successRate": 91.7,
    "successTrend": 0.0,
    "avgDurationMs": 28400,
    "durationTrend": 0.0
  },
  "trend": [ { "time": 1789948800, "value": 2 } ],
  "health": { "integrations": [], "llmProviders": [], "overall": "success", "lastChecked": "..." },
  "recentReviews": [
    {
      "id": "550e8400-...",
      "mrTitle": "Fix login",
      "project": "owner/repo",
      "author": { "name": "alice", "avatarUrl": null },
      "status": "success",
      "durationMs": 28400,
      "createdAt": "2026-07-18T02:00:00Z"
    }
  ]
}
```

---

### 9. Finding 反馈闭环

用户对单条 finding 打「有用 / 误报」标记，服务端按稳定 fingerprint 归并统计命中率与误报率，为后续 prompt 校准和降误报提供数据基础（设计见 `docs/professional_team_design.md` §6.3 / §8.9）。

**fingerprint**：对 `(file, line, title, category)` 做 SHA-256（字段间以 `0x1f` 分隔），取前 16 个 hex 字符。同一 finding 在多次评审中 fingerprint 不变。

**存储**：JSON 数组，默认 `~/.config/review-engine/feedback.json`，可用环境变量 `REVIEW_FEEDBACK_PATH` 覆盖；写入为原子写（tmp + rename）。

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
| **Web UI** | 用户启动 `review-engine serve`，前端 AJAX → `localhost:8080` | CORS + 无 auth（localhost） |
| **Desktop App** | 同机或内嵌启动 server，HTTP 通信 | 同上 + token auth 可选 |
| **VSCode Extension** | 方案 A（推荐）：extension 激活时 `review-engine serve --port 9123` 启动后台进程，通过 `localhost:9123/api/v1/*` 通信，extension 退出时 kill | 同 Web UI |
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
review-engine generate-token
# → review_a1b2c3d4e5f6g7h8i9j0k1l2m3n4o5p

# 安全启动方式
review-engine serve --bind 0.0.0.0 --api-token $(review-engine generate-token)
```

### 哪些路由需要认证

| 路由 | `127.0.0.1` | `0.0.0.0` | 原因 |
|------|------------|-----------|------|
| `GET /health` | 不认证 | 不认证 | 存活检查，无敏感信息 |
| `GET /api/v1/system/version` | 不认证 | 不认证 | 版本信息 |
| `GET /api/v1/system/experts` | 不认证 | 不认证 | expert 列表 |
| `POST /api/v1/config/validate` | 不认证 | 不认证 | 纯校验，无副作用 |
| `POST /api/v1/reviews` | 不认证 | **认证** | 消耗 LLM token，有成本风险 |
| `GET /api/v1/reviews` | 不认证 | **认证** | 可能泄漏代码 diff |
| `GET /api/v1/reviews/:id` | 不认证 | **认证** | 同上 |
| `GET /api/v1/config` | 不认证 | **认证** | 可能泄漏敏感配置 |
| `GET /api/v1/queue/stats` | 不认证 | 不认证 | 聚合统计，无敏感信息 |
| `GET /api/v1/queue/tasks` | 不认证 | **认证** | 可能包含 MR 标题与仓库信息 |
| `DELETE /api/v1/queue/tasks/:id` | 不认证 | **认证** | 控制任务状态 |
| `POST /api/v1/queue/tasks/:id/retry` | 不认证 | **认证** | 重新消耗 LLM token |
| `POST /api/v1/queue/pause` | 不认证 | **认证** | 控制队列状态 |
| `POST /api/v1/queue/resume` | 不认证 | **认证** | 控制队列状态 |
| `POST /api/v1/queue/max-concurrent` | 不认证 | **认证** | 修改并发配置 |

### 请求方式

支持两种方式传递 Token（客户端任选其一）：

```
# Bearer Token（标准 HTTP Auth）
Authorization: Bearer review_a1b2c3d4e5f6g7h8i9j0k1l2m3n4o5p

# X-API-Key Header
X-API-Key: review_a1b2c3d4e5f6g7h8i9j0k1l2m3n4o5p
```

### 与 GitLab webhook 的关系

已有独立的 `X-Gitlab-Token` 校验（`src/server/gitlab.rs`），两者不冲突：
- GitLab webhook → `X-Gitlab-Token` header（硬编码）
- API 请求 → `Authorization: Bearer` / `X-API-Key`（用户配置的 token）

```toml
[dependencies]
schemars = "0.8"        # 从 Rust struct 生成 JSON Schema
tower-http = { version = "0.6", features = ["cors"] }  # 已存在，需加 feature
uuid = { version = "1", features = ["v4"] }             # 已存在
serde = { version = "1", features = ["derive"] }        # 已存在
serde_json = "1"                                        # 已存在
```

现有 `Cargo.toml` 中 `tower-http` 和 `uuid` 已存在，只需确认 feature 开启。
