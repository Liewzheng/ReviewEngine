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
│  ├── repo.rs      POST/GET repo health scan               │
│  ├── config.rs    GET config/schema, POST validate         │
│  ├── system.rs    health, version, experts list            │
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

与 review 共用同一套 task 机制，但扫描只走本地文件系统，不需要 LLM，可快速返回。

#### `POST /api/v1/repo-scan`

```
Request:
{
  "path": "/path/to/repo"
}

Response 202:
{
  "task_id": "...",
  "status": "running"
}
```

#### `GET /api/v1/repo-scan/:task_id`

返回 `RepoHealthReport`（复用 `repo` 模块的输出）。

---

### 3. 配置

#### `GET /api/v1/config`

返回当前生效的 AppConfig（脱敏，隐藏所有 token 字段）。

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

### 4. 系统

#### `GET /api/v1/experts`

```
Response 200:
{
  "experts": [
    {
      "name": "security",
      "role": "Security Lead",
      "title": "Staff Security Engineer",
      "trigger": "always",
      "enabled": true
    }
  ]
}
```

VSCode Extension 可用此接口展示可选专家、让用户开关。

#### `GET /api/v1/version`

```
Response 200:
{
  "version": "0.6.10",
  "commit": "3be7ac1",
  "features": ["cli", "python"]
}
```

#### `GET /api/v1/health`

```
Response 200:
{
  "status": "ok"
}
```

---

### 5. 实时推送（SSE）

#### `GET /api/v1/events`

```
data: {"task_id":"...","status":"completed","event":"review.completed"}

data: {"task_id":"...","status":"running","event":"review.started"}
```

Web UI 和 Desktop App 通过 `EventSource` 监听，无需轮询。

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
| `src/server/api/types.rs` | TaskStatus, ReviewRequest 等 | 70 |
| `src/server/task_queue.rs` | 内存 TaskStore（`Arc<RwLock<HashMap>>`） | 100 |
| `src/server/auth.rs` | Bearer token 验证中间件 | ~30 |
| **合计** | | **~550 行** |

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

## 6. 认证策略

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
| `GET /api/v1/version` | 不认证 | 不认证 | 版本信息 |
| `GET /api/v1/experts` | 不认证 | 不认证 | expert 列表 |
| `POST /api/v1/config/validate` | 不认证 | 不认证 | 纯校验，无副作用 |
| `POST /api/v1/reviews` | 不认证 | **认证** | 消耗 LLM token，有成本风险 |
| `GET /api/v1/reviews` | 不认证 | **认证** | 可能泄漏代码 diff |
| `GET /api/v1/reviews/:id` | 不认证 | **认证** | 同上 |
| `GET /api/v1/config` | 不认证 | **认证** | 可能泄漏敏感配置 |

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
