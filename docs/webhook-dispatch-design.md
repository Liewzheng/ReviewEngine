---
title: Webhook 分发与评论去重设计方案
description: 并发 push 去重 + comment 更新机制的系统设计
tags:
  - webhook
  - architecture
  - dispatcher
  - publisher
related:
  - ../src/server/dispatcher.rs
  - ../src/server/gitlab.rs
  - ../src/git_provider/mod.rs
  - ../src/publisher/mod.rs
  - ../.notes/review_engine_rs_roadmap.md
---

# Webhook 分发与评论去重设计方案

> 对应 todo.md #95（并发 push 去重）和 #96（comment 去重与更新）

---

## 一、问题与现状

### 1.1 #95 — 并发 push 无去重

```
连续两次 push → 两个 tokio::spawn → 两次 LLM 调用 → 两条重复评论
```

`handle_mr_hook` 对 `open/reopen/update` 事件**每次都无条件 spawn**，没有检查：
- 当前 MR 是否已有 review 正在执行
- 当前 commit SHA 是否已经被审核过

### 1.2 #96 — 评论只创建不更新

`run_review_for_mr` 每次都调用 `post_mr_discussion` 发新评论，即使 MR 已经有一条 bot 评论。

`Publisher::update_discussion` 方法**已存在**，但从未被调用。

---

## 二、架构概览

```
GitLab Webhook
     │
     ▼
handle_mr_hook
     │
     ▼
┌─────────────────────────────────────┐
│         MrDispatcher                │  ← 新增
│  ┌─────────────────────────────┐    │
│  │  HashMap<MR_URL,           │    │
│  │    Arc<Mutex<MrStatus>>>   │    │
│  └─────────────────────────────┘    │
│  try_start() → ShouldStart         │
│  complete()                        │
│  wait()                            │
└──────────┬──────────────────────────┘
           │ Go
           ▼
    run_review_for_mr()
           │
           ▼
    ┌──────────────────────────────┐
    │  Publisher                   │  ← 修改
    │  ├ post_mr_discussion()      │
    │  ├ update_discussion()       │
    │  └ find_or_update() ──────►  │  ← 新增（默认: fallback to post）
    │                    GitLab:   │
    │                    1. list   │
    │                    2. match  │
    │                    3. update │
    │                       or new │
    └──────────────────────────────┘
```

---

## 三、模块设计

### 3.1 MrDispatcher — `src/server/dispatcher.rs`（新增）

#### 数据结构

```rust
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, Notify};

/// MR 分发去重器。跨 webhook 处理共享的单例。
#[derive(Clone)]
pub struct MrDispatcher {
    inner: Arc<Mutex<HashMap<String, Arc<Mutex<MrStatus>>>>>,
}

struct MrStatus {
    running: bool,
    last_sha: Option<String>,
    notify: Arc<Notify>,
}

/// try_start() 的返回结果
pub enum ShouldStart {
    /// 新工作，可以启动 review
    Go,
    /// 此 SHA 已审核过，跳过
    AlreadyReviewed,
    /// 当前有 review 正在运行，调用方应等待
    InProgress,
}
```

#### 核心逻辑

```rust
impl MrDispatcher {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 尝试启动 review。
    ///
    /// 线程安全：两层 Mutex，外层保护 HashMap 插入/删除，
    /// 内层保护单个 MR 的状态读写。避免粗粒度锁阻塞不相干 MR。
    pub async fn try_start(&self, mr_url: &str, sha: &str) -> ShouldStart {
        let mut map = self.inner.lock().await;
        let entry = map
            .entry(mr_url.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(MrStatus {
                running: false,
                last_sha: None,
                notify: Arc::new(Notify::new()),
            })));
        let mut status = entry.lock().await;

        if status.running {
            return ShouldStart::InProgress;
        }

        if status.last_sha.as_deref() == Some(sha) {
            return ShouldStart::AlreadyReviewed;
        }

        status.running = true;
        ShouldStart::Go
    }

    /// 标记 review 完成，记录 SHA，通知等待者。
    pub async fn complete(&self, mr_url: &str, sha: &str) {
        let map = self.inner.lock().await;
        if let Some(entry) = map.get(mr_url) {
            let mut status = entry.lock().await;
            status.running = false;
            status.last_sha = Some(sha.to_string());
            status.notify.notify_waiters();
        }
    }

    /// 等待当前 review 完成。调用方在收到 InProgress 后会调用此方法。
    pub async fn wait(&self, mr_url: &str) {
        let map = self.inner.lock().await;
        if let Some(entry) = map.get(mr_url) {
            let status = entry.lock().await;
            if status.running {
                let notify = status.notify.clone();
                drop(status);
                drop(map);
                notify.notified().await;
            }
        }
    }
}
```

#### 状态机

```
                    try_start(sha=sha_new)
  ┌────────────────────────────────────────┐
  │                                        ▼
  │  ┌──────────┐   Go    ┌───────────┐
  │  │  IDLE    │ ──────► │  RUNNING  │
  │  └──────────┘         └─────┬─────┘
  │       ▲                    │ complete()
  │       │                    ▼
  │       │             ┌───────────┐
  │       │             │ COMPLETED │
  │       │             └───────────┘
  │       │                    │
  │       │     try_start(sha=sha_same)
  │       │     ──► AlreadyReviewed
  │       │
  │       │     try_start(sha=sha_new)
  │       │     ──► Go (重新进入 RUNNING)
  │       │
  │       └── try_start while RUNNING
  │           ──► InProgress → wait() → 重试
```

#### 边界覆盖

| 场景 | 行为 |
|------|------|
| 同一 commit 来 2 次 | `AlreadyReviewed`，直接跳过 |
| 新 commit 来但 review 还在跑 | `InProgress` → 调用方 `wait()` → 等待完成后下一个 webhook 走正常流程 |
| Review panic 崩溃 | `complete()` 不会执行，`running` 保持 `true`，后续事件永远 InProgress |
| 服务重启 | 内存状态丢失，视为未审核（可接受，多一次 LLM 调用） |

> **关于 panic 安全与持久化（状态：v0.7.10，A10 已实现）**：review 失败/panic 时调用方执行 `dispatcher.reset()` 释放 `running` 锁，后续 webhook 可正常重试（`src/server/gitlab.rs:363`、`src/server/gitlab.rs:500`）；dispatcher 作为共享单例在 router 注入（`src/server/router.rs`）。A10 已实现超时恢复与磁盘持久化：`running` 带时间戳，超过 `REVIEW_DISPATCH_TIMEOUT_SECS`（默认 900s）判过期可重新发起；状态持久化到 `REVIEW_DISPATCH_STATE`（默认 `~/.config/review-engine/dispatcher-state.json`），重启后自动加载、过期 running 自动判过期。

---

### 3.2 修改 `src/server/gitlab.rs`

#### handle_mr_hook

```rust
async fn handle_mr_hook(
    State(dispatcher): State<MrDispatcher>,
    // ...
) -> Result<Json<Value>, StatusCode> {
    let sha = parsed["object_attributes"]["last_commit"]["id"]
        .as_str()
        .unwrap_or("");

    match dispatcher.try_start(&mr_url, sha).await {
        ShouldStart::Go => {
            let d = dispatcher.clone();
            let u = mr_url.clone();
            let s = sha.to_string();
            tokio::spawn(async move {
                if let Err(e) = run_review_for_mr(&url, &token, Some(&d), &u, &s).await {
                    tracing::error!("Review failed for MR !{}: {:?}", mr_iid, e);
                }
            });
        }
        ShouldStart::AlreadyReviewed => {
            tracing::info!("Skipping MR !{}: already reviewed at SHA {}", mr_iid, sha);
        }
        ShouldStart::InProgress => {
            tracing::info!("MR !{} review in progress, waiting...", mr_iid);
            dispatcher.wait(&mr_url).await;
        }
    }
    // ...
}
```

#### run_review_for_mr

接受可选 `dispatcher`，完成后调用 `complete()`：

```rust
async fn run_review_for_mr(
    mr_url: &str,
    gitlab_token: &str,
    dispatcher: Option<&MrDispatcher>,
    dispatch_key: &str,
    sha: &str,
) -> anyhow::Result<()> {
    // ... 现有代码 ...

    // 发布/更新评论（不再直接 post_mr_discussion）
    let publisher = GitLabPublisher::new(gitlab_token, mr_url)?;
    publisher.find_or_update_discussion(&md).await?;

    // 通知 dispatcher
    if let Some(d) = dispatcher {
        d.complete(dispatch_key, sha).await;
    }

    Ok(())
}
```

---

### 3.3 修改 `src/git_provider/mod.rs` — GitProvider trait

> **落地更正**：实际实现未新建 `Publisher` trait。`find_or_update_discussion`（含默认实现）与 `update_discussion` 直接加在现有 `GitProvider` trait 上（`src/git_provider/mod.rs:42-48`）；`src/publisher/mod.rs` 现仅保留 `InlineNote`、`publish_inline_notes` 等辅助函数。下文代码块为原设计，签名以 `GitProvider` trait 为准。

```rust
#[async_trait]
pub trait Publisher: Send + Sync {
    async fn post_mr_discussion(&self, body: &str) -> Result<String>;
    async fn post_inline_note(&self, note: &InlineNote) -> Result<()>;
    async fn update_discussion(&self, discussion_id: &str, body: &str) -> Result<()>;

    /// 查找已有 discussion 并更新，否则新建。
    /// 默认实现直接发布新讨论（不做查找）。
    /// GitLabPublisher 会覆盖此方法实现 find-or-create。
    async fn find_or_update_discussion(&self, body: &str) -> Result<String> {
        self.post_mr_discussion(body).await
    }
}
```

**为什么加到 trait 而不是直接在 gitlab.rs 硬编码**：
- 保持 `Publisher` 的完整性，未来 GitHub Publisher 也可以实现自己的 find-or-create 逻辑
- 默认实现向后兼容，不影响现有 mock 测试

---

### 3.4 修改 `src/git_provider/gitlab/mod.rs`（原设计为 `src/publisher/gitlab.rs`）

> **落地更正**：GitLab 侧的 `find_or_update_discussion` / `update_discussion` 实现位于 `impl GitProvider for GitLabProvider`（`src/git_provider/gitlab/mod.rs:58`、`:75`），GitHub 侧对应实现在 `src/git_provider/github/mod.rs:70`、`:92`。

```rust
const BOT_DISCUSSION_TITLE: &str = "# CodeReview Board";

#[async_trait]
impl Publisher for GitLabPublisher {
    // ... 现有方法 ...

    async fn find_or_update_discussion(&self, body: &str) -> Result<String> {
        let discussions = self.client.list_discussions().await?;

        for discussion in &discussions {
            if let Some(note) = discussion.notes.first() {
                if note.body.starts_with(BOT_DISCUSSION_TITLE) {
                    self.client.update_note(note.id, body).await?;
                    return Ok(note.id.to_string());
                }
            }
        }

        self.post_mr_discussion(body).await
    }
}
```

**设计决策**：用标题前缀匹配而非 bot 用户名匹配。
| 方案 | 优点 | 缺点 |
|------|------|------|
| 标题前缀 `# CodeReview Board` | 零额外 API 调用；标题本身唯一标识 | 如果用户手动改了标题前缀则不匹配 |
| 用户名匹配 | 不依赖内容 | 需要额外 `GET /user` 接口确定自己是谁；用户名可能变化 |

---

### 3.5 修改 `src/git_provider/gitlab/client.rs`

```rust
#[derive(Deserialize)]
pub struct Discussion {
    pub notes: Vec<DiscussionNote>,
}

#[derive(Deserialize)]
pub struct DiscussionNote {
    pub id: i64,
    pub body: String,
}

impl Client {
    pub async fn list_discussions(&self) -> Result<Vec<Discussion>> {
        let url = format!("{}/discussions", self.mr_base_url());
        let resp: Value = self.get(&url).await?;
        Ok(serde_json::from_value(resp)?)
    }
}
```

---

### 3.6 修改 `src/server/gitlab.rs` — WebhookState 持有 dispatcher

```rust
pub struct GitLabWebhookState {
    pub webhook_secret: String,
    pub dispatcher: MrDispatcher,
}
```

`serve()` 创建 `MrDispatcher` 并注入：

```rust
let dispatcher = MrDispatcher::new();
let ws_state = std::env::var("GITLAB_WEBHOOK_SECRET").ok().map(|secret| {
    gitlab::GitLabWebhookState { webhook_secret: secret, dispatcher: dispatcher.clone() }
});
```

---

## 四、文件变更清单

| 文件 | 操作 | 说明 | 行数 |
|------|------|------|------|
| `src/server/dispatcher.rs` | **新增** | MrDispatcher：try_start / complete / wait | ~80 |
| `src/server/gitlab.rs` | **修改** | handle_mr_hook 集成 dispatcher | +30 |
| `src/git_provider/mod.rs` | **修改** | GitProvider trait 新增 find_or_update_discussion（默认实现）与 update_discussion（原设计为 `src/publisher/mod.rs` 的 Publisher trait） | +5 |
| `src/git_provider/gitlab/mod.rs` | **修改** | 实现 find_or_update_discussion / update_discussion（原设计为 `src/publisher/gitlab.rs`） | +20 |
| `src/git_provider/gitlab/client.rs` | **修改** | 新增 list_discussions + Discussion 结构体 | +30 |
| `src/server/mod.rs` | **修改** | serve() 创建 dispatcher 注入 WebhookState | +3 |
| **合计** | | | **~168 行** |

---

## 五、不受此方案影响的边界

| 问题 | 理由 |
|------|------|
| 服务重启后重复审核 | 内存状态丢失，多发一次 LLM 请求。可接受。Phase 4 用持久化解决 |
| Push hook 触发 review | 当前 `handle_push_hook` 仅 log。未来如需启用，只需加 dispatcher 调用，不影响现有设计 |
| 多实例水平扩展 | 当前为单体架构。多实例场景下需用 Redis 共享状态，Phase 4 考虑 |

---

## 六、实施顺序

| 步骤 | 内容 | 前置 |
|------|------|------|
| 1 | `src/git_provider/gitlab/client.rs` — 新增 `list_discussions` + `Discussion` 结构体 | 无 |
| 2 | `src/git_provider/mod.rs` — GitProvider trait 新增 `find_or_update_discussion` 默认方法 | 无 |
| 3 | `src/git_provider/gitlab/mod.rs` — 实现 `find_or_update_discussion` | 1 |
| 4 | `src/server/dispatcher.rs` — MrDispatcher | 无 |
| 5 | `src/server/gitlab.rs` — handle_mr_hook 集成 dispatcher | 4 |
| 6 | `src/server/mod.rs` — serve() 创建并注入 dispatcher | 4 |
| 7 | 验证 + 测试 | 全部 |
