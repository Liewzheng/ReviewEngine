# 持久化设计（0.10.0）

> 状态：草案 v1（杜衡，2026-09-03）
> 范围：ReviewEngine 0.10.0 数据库持久化。本文只做设计，不含实现代码；SQL 为建表草案，可直接誊入 `migrations/`。

## 1. 目标与已拍板决策

以下决策已拍板，本文不重新论证，只在既有约束内做落地设计：

1. **部署形态**：PG 为主、SQLite 兜底。有 `DATABASE_URL` 走 PostgreSQL；无则内嵌 SQLite，默认路径 `~/.config/review-engine/review.db`。
2. **访问层**：sqlx 0.8，运行时 `Any` 池（features: `runtime-tokio`, `postgres`, `sqlite`, `migrate`, `chrono`, `uuid`, `json`）。
3. **评论回流**：GitLab Note webhook 实时入库为主，评审前主动拉取 notes API 兜底。
4. **配置入库**：git 平台 / LLM 实例配置从 `ui-state.toml` 搬进数据库，含一次性透明迁移。

0.10.0 要解决的具体问题：

- 重启后评审历史丢失（TaskStore 纯内存）。
- 配置持久化依赖单个 TOML 文件，无并发写保护、无历史语义。
- MR 讨论（人类评论 + 历史评审结论）不进评审上下文，二次评审重复劳动。
- LLM API key 目前明文落盘（`persist.rs:27-29` 明写的威胁模型例外），借入库一并收进加密边界。

## 2. 现状结论（已核实）

### 2.1 任务存储

- `src/server/task_queue.rs:116` `TaskStore`：`HashMap<Uuid, TaskEntry>` + broadcast SSE；`TaskEntry` 字段见 68-84 行（`task_id/state/created_at/started_at/completed_at/result/error/request/source_meta/progress/expert_name`）。
- reaper：每 300 s 清 `completed_at` 超 30 分钟的条目（135-149 行），手动路径 `cleanup_expired()`（160-167 行）。
- 状态机：`Pending → Running → (Completed | Failed)`，`Cancelled` 终态且 `update` 对其早退（256-261 行）。`retry` 仅允许 `Failed → Pending`（451-475 行）。
- 结果以 `serde_json::Value`（序列化的 `ReviewOutput`，`src/models/finding.rs:143`）挂 `result`；`ReviewOutput` 含 `reports: Vec<ExpertReport>`、`aggregated: Option<AggregatedReport>`、`consolidated: Option<ConsolidatedReport>`。

### 2.2 API 投影

- `src/server/api/review/task.rs:24-46` `task_to_status`、48-104 `build_review_detail`、106-124 `build_review_list_item`：`TaskEntry → API 响应` 的唯一转换点，入库后改造面集中在这三个函数与 `handlers.rs` 的 `list_reviews`（298 行）/`get_review`（207 行）。
- 分页参数结构 `ListParams` 已含 `status/page/per_page/q/project/repository/date_from/date_to`（task.rs:155-165），0.10.0 不需要新增参数，只需要换数据源。
- **搭车 bug 确认**：task.rs:75 `raw_comment` 只取 `output.aggregated.markdown`；团队评审 `aggregated=None` 时详情「完整评论」tab 空态。可用的 fallback 是 `output.consolidated.assessment.tl_dr`（`src/models/mod.rs:102`，`ConsolidatedReport` 结构见 `src/team/lead_consolidator.rs:62-80`）。

### 2.3 配置持久化

- `src/server/api/config/persist.rs`：`UiStateFile` 四区段（`ui` 投影 / `llm: Vec<LLMConfig>` / `git_platforms: Vec<GitPlatformConfig>` / `gitlab: PersistedGitlabConfig`，52-78 行）。`PUT /api/v1/config` 热生效 + 落盘；启动经同一 `apply_ui_config` 回放（366-375 行）。
- **env/CLI 来源值永不落盘**：`UiStateEnvOverrides`（346-361 行）+ `from_applied` 的 `is_env_derived_llm` / `strip_env_value` 过滤（93-178 行）。此原则入库后必须原样保留——入库只是换 `save_ui_state` 的落点，过滤逻辑不动。
- git 凭据已加密（`encrypt_ui_state`，231-242 行）；**LLM API key 明文**（`llm` 区段不在加密范围，27-29 行注释明写）。

### 2.4 加密边界

- `src/config/secrets.rs`：ChaCha20-Poly1305，`enc:` 前缀 + 配置目录 `secrets.key`（32 字节，0600，原子写）。`decrypt_secret` 对无 `enc:` 前缀的值透传（126-128 行），天然兼容遗留明文。
- 入库后加解密仍只在持久化边界发生，密钥文件位置不变（沿用 `key_path_for`，40-45 行）。

### 2.5 Webhook

- **更正任务描述的一处事实**：Note Hook 处理器已存在——`handle_note_hook`（`src/server/gitlab/hooks.rs:408`）目前用于 `/review`、`/describe` 命令触发评审，含 allowlist 门禁与 URL 重写。0.10.0 的新工作不是"新增 Note 事件类型"，而是**在既有处理器里加 note 入库**，并在评审 worker 侧消费。
- notes API 能力已具备：`list_discussions`（`src/git_provider/gitlab/client.rs:587`）、`post_note`（595）、`get_current_user_id`（144，回流自噬过滤要用）。

### 2.6 其他挂点

- `AppState`（`src/server/state.rs:208`）已有 `Option<Arc<...>>` 挂可插拔组件的先例（`task_store: Option<Arc<TaskStore>>` 216 行、`feedback_store` 239 行）。DB handle 沿用同一模式：`pub db: Option<Arc<SqlxStore>>`。
- `TaskStore::new()` 会被无 tokio runtime 的同步单测经 `AppState::new()` 触达（task_queue.rs:129-134 注释），DB 注入不能破坏这条路径——用 `Option` + setter，默认 `None` 即 0.9 行为。
- `Cargo.toml` 当前无 sqlx 依赖；`async-trait`、`chrono`、`uuid`、`serde_json` 均已在依赖树中。

## 3. Schema 定稿

单目录 `migrations/`，首版一个文件 `0001_init.sql` 建全部 7 表。sqlx `migrate!()` 宏内嵌，`Migrator::run(&pool)` 启动时执行。

### 3.1 方言差异点（设计约束，先于 DDL）

`Any` 池双后端共用同一套 SQL，以下约束逐条对应后面的 DDL 写法：

| 主题 | PG | SQLite | 本文的取舍 |
|---|---|---|---|
| 占位符 | 原生 `$1..$n` | `?` | **统一写 `?`**。Any 驱动内部为 PG 做翻译；写 `$1` 在 SQLite 端直接报错。（落地验证点 A，见 §11） |
| upsert | `ON CONFLICT ... DO UPDATE/NOTHING` | 同语法（≥3.24） | 两端一致，直接用；sqlx 内置 libsqlite3 版本远高于此 |
| `RETURNING` | 支持 | ≥3.35 支持 | **一律不用**。主键全部由 Rust 侧生成（UUID v4），写后无需回读；避免 Any 下两端 decode 行为差异 |
| JSON 列 | 原生 JSONB | TEXT | **DDL 用 TEXT，绑定用 `String`**：store 层 `serde_json::to_string` 后按 TEXT 绑定，读出再 `from_str`。若声明 PG JSONB 列而 SQLite 是 TEXT，`serde_json::Value` 在 PG 端会按 JSONB 编码、绑到 TEXT 列报类型错——应用层序列化是唯一两头都稳的做法 |
| 布尔 | 原生 BOOL | 0/1 | DDL `BOOLEAN`，sqlx Any 的 `bool` 编解码两端兼容 |
| 时间戳 | TIMESTAMPTZ | 无原生类型（NUMERIC 亲和） | DDL `TIMESTAMP`；**值一律 Rust 侧 chrono 生成**，不写 `CURRENT_TIMESTAMP` 默认值，两端时间戳格式由应用层统一 |
| 模糊搜索 | `ILIKE` | `LIKE` 仅 ASCII 不敏感 | 统一 `LOWER(col) LIKE LOWER(?)`，行为两端一致 |
| 外键 | 默认启用 | 需 `PRAGMA foreign_keys=ON` | SQLite 连接串带 `?...` 参数或建池后执行 PRAGMA（见 §4.3） |
| 自增主键 | SERIAL/IDENTITY | AUTOINCREMENT | **都不用**：全部自然键/UUID 文本主键，绕开方言差异 |

### 3.2 建表 SQL 草案（`migrations/0001_init.sql`）

```sql
-- ── 评审任务（TaskEntry 的持久投影）──
CREATE TABLE reviews (
    task_id       TEXT PRIMARY KEY,            -- UUID v4, Rust 侧生成
    state         TEXT NOT NULL,               -- pending|running|completed|failed|cancelled
    source_meta   TEXT NOT NULL DEFAULT '{}',  -- SourceMeta JSON
    -- 从 source_meta 物化的过滤列：分页过滤要走索引，JSON 文本抽取两端写法不同，
    -- 写穿时由 Rust 同步维护，读路径不碰 JSON 抽取函数。
    project       TEXT,
    repository    TEXT,
    request       TEXT,                        -- 序列化 ReviewRequest（无凭据，见 task.rs:175-178）
    result        TEXT,                        -- ReviewOutput JSON
    error         TEXT,
    progress      INTEGER,                     -- 0-100，仅终态时快照；进行中的实时进度不入库
    created_at    TIMESTAMP NOT NULL,
    started_at    TIMESTAMP,
    completed_at  TIMESTAMP
);
CREATE INDEX idx_reviews_created_at ON reviews (created_at DESC);
CREATE INDEX idx_reviews_state      ON reviews (state);
CREATE INDEX idx_reviews_project    ON reviews (project);

-- ── 专家子报告（从 ReviewOutput.reports 拆行，便于按专家查询）──
CREATE TABLE expert_reports (
    task_id     TEXT NOT NULL REFERENCES reviews(task_id) ON DELETE CASCADE,
    expert_name TEXT NOT NULL,
    report      TEXT NOT NULL,                 -- ExpertReport JSON
    duration_ms INTEGER,                       -- 首版可为 NULL：TaskEntry 目前不记 per-expert 耗时，
                                               -- 需执行器补计时后再填充（见 §5.4 注意点）
    created_at  TIMESTAMP NOT NULL,
    PRIMARY KEY (task_id, expert_name)
);

-- ── MR 讨论（Note webhook + notes API 兜底共用的幂等存储）──
CREATE TABLE mr_discussions (
    platform   TEXT NOT NULL,                  -- GitPlatformConfig.name（实例级隔离）
    project    TEXT NOT NULL,                  -- path_with_namespace
    mr_iid     BIGINT NOT NULL,
    note_id    BIGINT NOT NULL,
    author     TEXT NOT NULL DEFAULT '',
    body       TEXT NOT NULL,
    created_at TIMESTAMP NOT NULL,             -- note 的创建时间，非入库时间
    ingested_at TIMESTAMP NOT NULL,            -- 入库时间，排序兜底
    PRIMARY KEY (platform, project, mr_iid, note_id)  -- 幂等键
);
CREATE INDEX idx_mr_discussions_mr ON mr_discussions (platform, project, mr_iid, created_at);

-- ── 注入上下文（支撑 LLM 前缀缓存复用）──
CREATE TABLE review_contexts (
    task_id        TEXT NOT NULL REFERENCES reviews(task_id) ON DELETE CASCADE,
    kind           TEXT NOT NULL,              -- 'mr_discussions' | 未来扩展
    content        TEXT NOT NULL,              -- 渲染后的上下文本（前缀稳定）
    content_hash   TEXT NOT NULL,              -- sha256 hex；同 MR 二次评审 hash 相同即复用
    token_estimate INTEGER NOT NULL DEFAULT 0,
    created_at     TIMESTAMP NOT NULL,
    PRIMARY KEY (task_id, kind)
);
CREATE INDEX idx_review_contexts_hash ON review_contexts (content_hash);

-- ── git 平台实例（ui-state.toml 的 [[git_platforms]] 区段入库）──
CREATE TABLE git_platforms (
    id                     TEXT PRIMARY KEY,   -- UUID v4；业务合并键仍是 name（与内存模型一致）
    name                   TEXT NOT NULL UNIQUE,
    type                   TEXT NOT NULL DEFAULT 'gitlab',
    base_url               TEXT NOT NULL DEFAULT '',
    internal_base_url      TEXT NOT NULL DEFAULT '',
    token                  TEXT NOT NULL DEFAULT '',  -- enc: 加密
    webhook_secret         TEXT NOT NULL DEFAULT '',  -- enc: 加密
    webhook_signing_secret TEXT NOT NULL DEFAULT '',  -- enc: 加密
    enabled                BOOLEAN NOT NULL DEFAULT TRUE,
    raw                    TEXT NOT NULL DEFAULT '{}',  -- 扩展兜底：allowed_projects 等未列化字段
    updated_at             TIMESTAMP NOT NULL
);

-- ── LLM 实例（[[llm]] 区段入库；api_key 顺带收进加密边界）──
CREATE TABLE llm_providers (
    id           TEXT PRIMARY KEY,             -- UUID v4
    provider     TEXT NOT NULL,                -- 对齐 LLMConfig.provider（brief 中的 "name"）
    model        TEXT NOT NULL DEFAULT '',
    api_base     TEXT NOT NULL DEFAULT '',
    api_key      TEXT NOT NULL DEFAULT '',     -- enc: 加密（新增：0.9 明文落盘）
    max_tokens   INTEGER NOT NULL DEFAULT 4096,
    temperature  REAL NOT NULL DEFAULT 0.7,
    raw          TEXT NOT NULL DEFAULT '{}',   -- 扩展兜底：disable_thinking 等
    updated_at   TIMESTAMP NOT NULL
);
CREATE UNIQUE INDEX idx_llm_providers_provider ON llm_providers (provider);

-- ── 应用设置（ui 投影 / legacy gitlab 字段 / rules / advanced 等）──
CREATE TABLE app_settings (
    key        TEXT PRIMARY KEY,               -- 如 'ui'、'gitlab'、'rules'、'advanced'
    value      TEXT NOT NULL,                  -- JSON
    updated_at TIMESTAMP NOT NULL
);
```

说明：

- legacy `gitlab` 三个凭据（`PersistedGitlabConfig`）进 `app_settings`（key=`gitlab`，值 JSON，三个字段均 `enc:`），不开新表——它是遗留域，未来会被 `git_platforms` 吸收。
- `git_platforms.id` / `llm_providers.id` 用 UUID 而非自增，原因见 §3.1 自增主键行。
- `reviews.request` 沿用现有约定：序列化的是无凭据 `ReviewRequest`，token 永不入库（task.rs:175-178 注释承诺的语义，入库后不变）。

## 4. Rust 抽象层设计

### 4.1 模块结构

```
src/store/
  mod.rs       — SqlxStore::connect(url) / ::connect_default()、方言探测、测试 helper（new_in_memory）
  traits.rs    — ReviewStore / ConfigStore / DiscussionStore 三个 trait
  sqlx.rs      — SqlxStore { pool: AnyPool } 及三个 trait 的实现；所有 SQL 集中在此文件
  rows.rs      — 行结构 ⇄ 领域结构（TaskEntry/UiStateFile/…）的编解码；enc: 加解密边界在此
migrations/
  0001_init.sql
```

`src/lib.rs` 加 `pub mod store;`。`AppState` 加 `pub db: Option<Arc<SqlxStore>>`（沿用 `task_store` 的 Option 先例，state.rs:216）。

### 4.2 trait 取舍：三个域 trait，一个实现

**推荐**：`ReviewStore`（reviews / expert_reports / review_contexts）、`ConfigStore`（git_platforms / llm_providers / app_settings）、`DiscussionStore`（mr_discussions）三个 trait，由同一个 `SqlxStore` 实现，共享一个 `AnyPool` 和 §3.1 的方言封装。

理由：

- 三类调用方天然不相交：task_queue 只碰评审域、config put/replay 只碰配置域、note hook / worker 只碰讨论域。按域拆分后每个调用方只见自己的方法面，单测 mock 面最小。
- 一个 `SqlxStore` 实现三者，避免了"每表一个 Repo"的样板爆炸（7 表 7 trait 没有收益）。
- 项目已有 `async-trait` 依赖（Cargo.toml:103），trait object 的装箱开销不在热路径上（热路径仍是内存 HashMap + SSE，见 §5）。

**否决的备选**：

- **单一大 `Store` trait**：任何一域加方法都动全局接口；mock 一个域要实现全部方法，测试成本高。否决。
- **不用 trait、调用方直接依赖具体 `SqlxStore`**：这是最简方案，差点入选。否决原因是两处调用方（`PUT /config` 持久化、note hook 幂等入库）的单测需要注入假实现来断言"写库被调用且内容正确"，若绑死具体类型就只能起真 DB。In-memory SQLite 能缓解但消不掉（连接池时序、加解密边界都要真跑），保留 trait 的成本很低。
- **每表一个 Repo trait**：过度碎片化，否决。

### 4.3 `sqlx::Any` 双后端可行性

结论：可行，但必须守住 §3.1 的封装纪律。落地要点：

- 连接：`AnyPoolOptions` + `sqlx::any::install_default_drivers()`；`DATABASE_URL` 存在且以 `postgres://`/`postgresql://` 开头 → PG；否则 SQLite。**`DATABASE_URL` 设置了但连接失败 → 启动显式报错退出，绝不静默落 SQLite**（数据写到意外的地方比启动失败更难收拾，见 §9）。
- SQLite URL 组装：默认 `sqlite://{config_dir}/review.db?mode=rwc`，`config_dir` 沿用 `resolve_ui_state_path` 的同套解析（persist.rs:214-225）；建池后执行 `PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;`。
- 迁移：`sqlx::migrate!("./migrations")` + `Migrator::run(&pool)`。**AnyPool 上的 migrate 支持需要落地时先跑 smoke test 确认**（验证点 A）：建空库跑 `0001_init.sql`，再跑一次确认幂等跳过。
- 测试 helper：`SqlxStore::new_in_memory()` 用 `sqlite::memory:` + `max_connections(1)`（连接池 >1 时每个连接是独立的内存库，这是 SQLite 内存模式的经典坑），供不写文件的单元测试使用。

## 5. TaskStore 写穿方案

### 5.1 原则

内存仍是热路径与 SSE 的唯一来源；DB 是历史的唯一来源。状态迁移**同步 await 写库**（保证重启恢复语义正确），写库失败不阻塞评审。

### 5.2 逐方法写穿点

`TaskStore` 新增 `db: Option<Arc<dyn ReviewStore>>`（构造后 setter 注入，`None` 即 0.9 纯内存行为，同步单测路径不受影响）：

| 方法（task_queue.rs 行号） | 写库动作 | 说明 |
|---|---|---|
| `create_with_request`（179） | INSERT reviews（state=pending） | |
| `start`（212） | UPDATE state=running, started_at | |
| `set_progress`（229） | **不写库** | 高频事件，进度对历史无价值；终态写时快照一次 `progress` 即可 |
| `fill_source_meta`（310） | UPDATE source_meta + 物化 project/repository | 每任务至多一次，值得写 |
| `update` 终态分支（249） | UPDATE state/result/error/completed_at/progress + 逐条 INSERT expert_reports | Cancelled 早退分支（259-261）不写 |
| `delete`（428，cancel 语义） | UPDATE state=cancelled, completed_at | |
| `retry`（451） | UPDATE state=pending, error=NULL, completed_at=NULL | |

写库失败处理：记 `tracing::error!` + 继续。终态写失败做一次立即重试，仍失败则放过——历史页少一条可接受，评审本身不能死。

### 5.3 重启恢复语义

- 启动时（migrate 之后、HTTP 监听之前）执行：`UPDATE reviews SET state='failed', error='interrupted: server restarted', completed_at=? WHERE state IN ('pending','running')`。
- **取舍：复用 `failed` 而非新增 `interrupted` 状态**。新增 `TaskState::Interrupted` 会涟漪到 `task_status_str`、SSE 事件映射、前端 StatusBadge 颜色表（design.md §6.1），而收益只是列表上一个标签差异；`error` 文案已能表达原因。前端若想区分，可读 `error` 前缀。
- 中断任务**不自动重新入队**：自动重跑会消耗 LLM 配额且可能重复评论 MR；由用户在历史页手工 retry（`retry` 允许 `Failed → Pending`，interrupted 落库为 failed，天然可 retry）。
- 终态从库读：历史列表/详情直接查 DB（写穿保证 DB 含进行中任务），内存从空启动，无需回填。

### 5.4 reaper 与持久化的关系

- 30 分钟 reaper **保持原样、只清内存**（task_queue.rs:135-149），不删库。队列监控/SSE 的视图不变。
- 注意点：`expert_reports.duration_ms` 首版允许 NULL——`TaskEntry` 不记 per-expert 耗时，执行器补计时是独立小改动，不阻塞本方案。

## 6. 配置迁移方案

### 6.1 启动序列（严格按序）

1. 解析 DB URL（§4.3）→ 建池 → `Migrator::run` → **失败即退出非零**（§9）。
2. 恢复语义扫尾（§5.3 的 interrupted UPDATE）。
3. **一次性导入**：`git_platforms`、`llm_providers`、`app_settings` 三表合计为空 且 `ui-state.toml` 存在 → 走现有 `load_ui_state`（persist.rs:310，含解密）读入 → 经 `rows.rs` 加密边界写库（git 凭据 + LLM key 全部 `enc:`）→ `std::fs::rename("ui-state.toml", "ui-state.toml.migrated")`。**备份不删**。
   - 导入失败：记 error、**不改名原文件**、回退到现有 `load_and_apply_ui_state` 文件回放路径继续启动——迁移失败不能让用户丢配置。
   - 导入成功的判定要保守：三表全部写入完成才 rename；任何一步失败整体回滚（单事务包裹整个导入）。
4. 之后经同一 `apply_ui_config` 路径从 DB 回放（替换 `load_and_apply_ui_state` 的数据源，回放逻辑本身不动——热/冷语义一致性是现有设计的优点，保留）。

### 6.2 PUT /config 语义

- 热生效路径（`apply_ui_config`）完全不变。
- 持久化落点从 `save_ui_state`（文件）换成 `ConfigStore::save_*`（库）。**`UiStateFile::from_applied` 的 env 过滤逻辑原样复用**（persist.rs:93-178）：env/CLI 来源值永不入库，与永不落盘同一原则。
- 落库失败：返回 500（与今天 `save_ui_state` 失败一致），不静默吞掉。
- `secrets.key` 位置不变（配置目录下）；`rows.rs` 用 `load_or_create_key` 拿同一把钥匙。PG 部署时 key 文件仍在 server 本地配置目录——这是本地对称加密的既有威胁模型，本文不扩大也不缩小它。

### 6.3 优先级矩阵（逐配置域）

| 配置域 | config.toml | DB（ui-state 迁入） | env/CLI |
|---|---|---|---|
| legacy gitlab 凭据（token/webhook_secret/signing_secret） | 仅作初始种子 | **权威源**；空时才用 env 兜底并记 deprecation warn | fallback-only（语义同今天，persist.rs:390-436） |
| LLM provider 列表 | 初始种子 | 覆盖 config.toml | **整体胜出**（`llm_from_env` 时 DB 的 llm 区段不回放，同 persist.rs:443-472） |
| git_platforms | 无来源（待核实：`config/resolver/` 是否承载 platforms，实现前确认） | **唯一权威** | 无来源 |
| ui 投影（rules / advanced / URL / 模型选择） | 初始种子 | 回放覆盖种子 | 无 |

迁移完成后 `UiStateEnvOverrides` 机制保留原名原义，只是过滤的落点从文件换成库。

## 7. Note webhook 入库 + 评审前注入

### 7.1 入库（挂在既有 `handle_note_hook`，hooks.rs:408）

在解析之后、命令判断之前插入入库逻辑（命令评论也是讨论历史的一部分，同样入库）：

- **payload 关键字段**：`object_kind`（须为 `"note"`）、`object_attributes.id`（note_id）、`object_attributes.noteable_type`（须为 `"MergeRequest"`，Commit/Issue/Snippet note 忽略）、`object_attributes.note`（body）、`object_attributes.created_at`、`user.username`/`user.name`（author）、`merge_request.iid`（缺失时回退 `object_attributes.url` 尾部解析，复用 `mr_iid_from_url`，hooks.rs:394）、`project.path_with_namespace`。platform 取匹配到的 `GitPlatformConfig.name`，未匹配用 `"default"`。
- **幂等**：主键 `(platform, project, mr_iid, note_id)`，`ON CONFLICT DO UPDATE SET body=excluded.body, author=excluded.author`——webhook 重投自然去重，note 被编辑则更新。
- **回流自噬防护（必须做）**：本服务自己 `post_note`/`post_comment` 发的评审报告也会触发 Note hook。不入库规则：(a) `object_attributes.note` 以本服务报告固定前缀开头；(b) 或 author id 等于 `get_current_user_id()`（client.rs:144）的结果（启动时解析一次并缓存）。两条件任一命中即跳过入库（命令触发的 `/review` note 除外——那是用户意图）。实现时确认 (a) 的报告前缀常量位置。
- 系统 note（`object_attributes.system=true`，如 "added 1 commit"）**入库但打标意义不大**——按已拍板 schema 无 `system` 列，决策：**跳过 system note**，它们是噪音不是讨论。

### 7.2 评审前注入（worker 侧）

在评审流水线取 diff 之后、专家执行之前（`resolve.rs` / `run_review_common` 路径）：

1. 按 `(platform, project, mr_iid)` 查 `mr_discussions`。
2. **兜底**：查询结果为空（或该 MR 从未见过）→ 调 notes API（`list_discussions`，client.rs:587）拉全量 → upsert 入库 → 用拉取结果。
3. **组织成追加式上下文**：按 `(created_at, note_id)` 升序渲染确定性模板，固定头部（如 `## MR Discussion History`）+ 逐条 `- [author @ created_at]: body`；body 截断上限（建议 2000 字符/条）防爆 context。**前缀稳定是硬要求**：同一 MR 历史不变时渲染输出逐字节相同，`content_hash` 相同 → LLM 前缀缓存命中；新评论只追加在尾部。
4. 渲染结果 + hash 写入 `review_contexts`（`ON CONFLICT (task_id, kind) DO UPDATE`）；hash 相同的后续任务可直接复用渲染文本。
5. **降级**：DB 不可用、notes API 失败、渲染超限——全部只记 warn，评审继续，不带讨论上下文。评论注入是增强，不是评审的前置条件。

## 8. API / 前端影响面

### 8.1 后端

- `list_reviews`（handlers.rs:298）：数据源从 `TaskStore.list`（内存）换为 DB 查询。**参数与响应 shape 不变**（`ListParams` 已齐，task.rs:155-165）：`page` 默认 1、`per_page` 默认 20、上限 100；`q` 用 `LOWER(source_meta) LIKE LOWER(?)`（§3.1）；`project`/`repository` 走物化列等值；`date_from/to` 走 `created_at` 范围；`COUNT(*)` 出 total。进行中任务 DB 已有（写穿），无需内存合并。
- `get_review`（handlers.rs:207)：改读 DB；若该 task_id 恰在内存中（进行中），叠加实时 `progress`/`expert_name` 两个字段后返回。`task_to_status`/`build_review_detail`/`build_review_list_item` 三个投影函数改为接受"DB 行结构"，签名变化收敛在 `api/review/task.rs` 一个文件。
- 新增（如评审前注入需要暴露）：无。Note 数据 0.10.0 不开查询 API。

### 8.2 前端

- 历史页（`/history`，design.md §2）：沿用服务端分页 + `ElPagination`（total 已有），每页 20。首版不做无限滚动。
- 若要滚动加载（可选增强）：IntersectionObserver 哨兵 div + page 累加 append 到列表；filter/q 变化时重置 page=1 并清空已加载；SSE 的 `review.completed` 事件触发第一页刷新而非整表重载（配合 design.md §7.5 的 flash-border）。
- 详情页：无结构变化（字段不变），但历史记录现在重启后仍在，注意加载态/404 处理走既有约定（design.md §10）。

### 8.3 搭车修复：团队评审详情空态

`build_review_detail`（task.rs:75）`raw_comment` fallback 链改为：

```
output.aggregated.map(|a| a.markdown)
    .or_else(|| output.consolidated.map(|c| c.assessment.tl_dr))
    .filter(|s| !s.is_empty())
```

`tl_dr` 字段已确认存在（`src/models/mod.rs:102`）。加一条 `aggregated=None + consolidated=Some` 的单测。

## 9. 风险与回退

| 风险 | 行为 | 回退 |
|---|---|---|
| `DATABASE_URL` 已设但 PG 连不上 | **启动显式报错退出**，绝不静默落 SQLite（数据写到意外的库比不起服务更糟） | 修好连接或显式去掉 `DATABASE_URL` 走 SQLite |
| SQLite 文件不可写（权限/只读盘） | 同样显式报错退出；提供逃生门 `REVIEW_DISABLE_DB=1`（或 `--no-db`）降级为 0.9 纯内存模式，启动时 warn 一条"持久化已禁用" | 设逃生门即回到 0.9 行为 |
| migrate 失败（SQL 写错、库损坏） | 退出非零，不启动 HTTP；DB 未被业务写入 | 0.9.x 二进制不读库，直接回退部署无副作用 |
| ui-state 导入中途失败 | 单事务回滚，原文件**不改名**，回退文件回放路径继续启动 | 下次启动重试导入（幂等：三表为空才触发） |
| `secrets.key` 丢失 | DB 中 `enc:` 值无法解，启动报错指明重录（沿用 secrets.rs 现有错误文案风格） | Web UI 重新录入凭据保存即可 |
| 写穿失败（内存成功、库失败） | error 日志 + 终态一次重试；评审不阻塞 | 历史页少记录，无其他影响 |
| 回滚到 0.9.x | — | DB 文件/PG 表原样保留无害；`ui-state.toml.migrated` 手工改回 `ui-state.toml` 即恢复旧配置源 |

## 10. 实施清单（依赖序，可逐项验收）

1. **[祁远]** `Cargo.toml` 加 sqlx 0.8（指定 features）；`src/store/` 骨架 + `migrations/0001_init.sql`；`SqlxStore::connect/new_in_memory` + migrate 接线。**验收**：验证点 A（Any 占位符翻译 + AnyPool migrate smoke test）通过，SQLite 内存库建表成功。
2. **[祁远]** `rows.rs` 加密边界 + `ConfigStore` 实现（§3.2 三张配置表 + §6.2 保存路径）。**验收**：配置 PUT→库→重启回放 round-trip 单测绿；LLM key 在库里是 `enc:`。
3. **[祁远]** 一次性导入（§6.1 第 3 步，单事务 + rename 备份 + 失败回退）。**验收**：老 `ui-state.toml`（含明文 LLM key）启动一次后：库里有数据、文件改名、GET /config 行为不变、env 覆盖矩阵（§6.3）逐行单测。
4. **[梁序]** `ReviewStore` + TaskStore 写穿（§5.2）+ 重启恢复（§5.3）。**验收**：跑一个评审 → kill -9 → 重启 → 该任务在库里是 failed/interrupted 文案；完成的评审重启后历史可查。
5. **[梁序]** `list_reviews`/`get_review` 读库 + 投影函数签名收敛（§8.1）。**验收**：分页/过滤参数行为与 0.9 一致（同参数响应 shape 不变）。
6. **[梁序]** Note hook 入库（§7.1，含自噬防护）+ worker 注入（§7.2）。**验收**：发 note webhook → 库里有行；重投不重复；编辑则更新；二次评审的 prompt 前缀逐字节稳定（hash 相同）。
7. **[沈一帆]** 前端历史页适配（§8.2）+ 详情空态修复的 UI 确认。**验收**：重启后历史页有数据；团队评审详情「完整评论」tab 非空。
8. **[梁序]** `build_review_detail` fallback 链（§8.3）+ 单测。
9. 全量：fmt / clippy / test 绿；PG 与 SQLite 双后端各跑一遍验收清单。

依赖关系：1 → 2,3,4；4 → 5,6；2,3 与 4,5 可并行；6 依赖 1 即可起步（`DiscussionStore` 独立），注入部分依赖 4 的 worker 改造对齐。

## 11. 待验证点（实现前确认，不确定处不猜）

- **验证点 A**：sqlx 0.8 `Any` 驱动的 `?` 占位符 PG 翻译行为、以及 `Migrator` 在 `AnyPool` 上的行为（含 `_sqlx_migrations` 锁表在 SQLite 上的表现）。方法：步骤 1 的 smoke test，双后端各跑。
- **验证点 B**：`config/resolver/` 是否从 config.toml 承载 `git_platforms`（§6.3 表中标注待核实）。方法：`Grep "git_platforms" src/config/`。
- **验证点 C**：评审报告的固定前缀常量位置（§7.1 自噬防护条件 a）。方法：`Grep` publisher/output 模块的报告头部模板。
- **验证点 D**：`Any` 驱动下 `chrono::DateTime<Utc>` 绑到 SQLite `TIMESTAMP` 列的存储格式与排序正确性（字典序 = 时间序是分页 `ORDER BY created_at` 的前提）。方法：步骤 4 的 round-trip 测试里断言排序。

## 12. 验收标准清单

- [ ] `cargo fmt --check` / `cargo clippy` / `cargo test` 全绿
- [ ] PG 与 SQLite 双后端：评审完成后历史落库、可查
- [ ] 重启 server 后历史列表/详情仍可查（§5.3）
- [ ] `ui-state.toml` 迁移后：配置热生效不变、密钥可用（git token 解密、LLM key 解密且库里为 `enc:`）、原文件备份为 `.migrated`
- [ ] Note Hook 入库：重投幂等、编辑更新、自身评论不入库
- [ ] 二次评审注入：prompt 中含讨论历史前缀，同 MR 无新评论时 `content_hash` 相同（前缀缓存可命中）
- [ ] 团队评审（`aggregated=null`）详情「完整评论」tab 不再空态
- [ ] `DATABASE_URL` 指向不可达 PG 时启动显式报错（不静默落 SQLite）
