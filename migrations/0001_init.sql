-- 0.10.0 持久化首版：7 表。方言约束见 design/persistence.md §3.1：
--   占位符统一 `?`；不用 RETURNING（主键 Rust 侧 UUID）；JSON 列一律 TEXT。
-- 时间戳列：与设计文档 §3.2 草案的 TIMESTAMP 不同，一律 TEXT，存 Rust 侧
--   chrono 生成的固定宽度 RFC 3339 UTC 串（2026-09-03T10:00:00.000000Z）。
-- 布尔列：同理不用 BOOLEAN，一律 INTEGER 0/1。
--   原因（验证点 A/D 落地结论）：sqlx Any 驱动对 SQLite 只认
--   Null/Int4/Integer/Float/Blob/Text 五类声明类型，BOOLEAN / TIMESTAMP
--   列读不出来，chrono/bool 也没有 Type<Any> 实现；固定宽度 UTC 串
--   字典序 == 时间序，ORDER BY / 范围过滤语义不变。PG 端 TEXT / INTEGER
--   列天然接受这些值，无需 CAST。

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
    created_at    TEXT NOT NULL,
    started_at    TEXT,
    completed_at  TEXT
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
    created_at  TEXT NOT NULL,
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
    created_at TEXT NOT NULL,             -- note 的创建时间，非入库时间
    ingested_at TEXT NOT NULL,            -- 入库时间，排序兜底
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
    created_at     TEXT NOT NULL,
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
    -- 设计文档 §3.1 写的是 BOOLEAN，但 Any 驱动无法解码 SQLite 声明类型为
    -- Bool 的列（验证点 A 实证；Any 只认 Null/Int4/Integer/Float/Blob/Text），
    -- 故用 INTEGER 0/1，绑定侧转 bool。
    enabled                INTEGER NOT NULL DEFAULT 1,
    raw                    TEXT NOT NULL DEFAULT '{}',  -- 扩展兜底：allowed_projects 等未列化字段
    updated_at             TEXT NOT NULL
);

-- ── LLM 实例（[[llm]] 区段入库；api_key 顺带收进加密边界）──
CREATE TABLE llm_providers (
    id           TEXT PRIMARY KEY,             -- UUID v4
    provider     TEXT NOT NULL,                -- 对齐 LLMConfig.provider（brief 中的 "name"）
    model        TEXT NOT NULL DEFAULT '',
    api_base     TEXT NOT NULL DEFAULT '',
    api_key      TEXT NOT NULL DEFAULT '',     -- enc: 加密（新增：0.9 明文落盘）
    max_tokens   INTEGER NOT NULL DEFAULT 4096,
    -- 浮点列：必须 DOUBLE PRECISION（float8），不能用 REAL——PG 的 REAL 是
    -- float4，而 store 层按 f64 绑定/解码（Any 驱动类型精确匹配，无隐式
    -- 解码转换），REAL 列读回直接 mismatched types（岑静 PG E2E 实测）。
    -- SQLite 端 DOUBLE PRECISION 同样落 REAL affinity（8 字节），两端兼容。
    temperature  DOUBLE PRECISION NOT NULL DEFAULT 0.7,
    raw          TEXT NOT NULL DEFAULT '{}',   -- 扩展兜底：disable_thinking 等
    updated_at   TEXT NOT NULL
);
CREATE UNIQUE INDEX idx_llm_providers_provider ON llm_providers (provider);

-- ── 应用设置（ui 投影 / legacy gitlab 字段 / rules / advanced 等）──
CREATE TABLE app_settings (
    key        TEXT PRIMARY KEY,               -- 如 'ui'、'gitlab'、'rules'、'advanced'
    value      TEXT NOT NULL,                  -- JSON
    updated_at TEXT NOT NULL
);
