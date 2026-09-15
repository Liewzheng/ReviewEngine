-- 0.10.23 (RENG-57)：LLM 调用级延迟采样。
-- 目的：LLM 状态页的「平均延迟」要从探测（RENG-36，瞬时值）变成真实的历史
-- 统计量，并且卡片上的延迟 sparkline 要有真实数据来源。此前没有任何地方记录
-- 逐次调用的耗时，页面只能显示探测值 + `—`。
-- 方言约束与 0001/0002/0003 一致：
--   * 时间戳一律 TEXT（Rust 侧 chrono 生成的定宽 RFC 3339 UTC 串），
--     字典序 == 时间序，范围过滤与 ORDER BY 语义不变；
--   * 布尔一律 INTEGER 0/1（Any 驱动不认 SQLite 的 BOOLEAN 声明类型）；
--   * JSON 一律 TEXT（本表不含 JSON 列，仅作说明）；
--   * 不使用 RETURNING（主键 Rust 侧生成）。
--
-- 无外键：`review_id` 在 webhook 路径上是运行时评审 id（`reviews.task_id`
-- 通常存在，但「无 DB 任务行」的评审也会走到这里），外键会让采样写入失败，
-- 而采样是 best-effort 的旁路数据，绝不能反过来影响评审本身（同 0002 的
-- 无外键理由）。幂等：重复写入是允许的（一次调用一行），不需要唯一键。

-- ── 逐次 LLM 调用采样 ──
CREATE TABLE llm_call_samples (
    id             TEXT PRIMARY KEY,           -- UUID v4，Rust 侧生成（代理键）
    review_id      TEXT,                       -- 归属的评审 id；未知时为 NULL
    provider       TEXT NOT NULL,              -- 命中的 config.provider（与 llm_summary 同源）
    model          TEXT NOT NULL DEFAULT '',   -- 命中的 config.model
    latency_ms     INTEGER NOT NULL,           -- 该次调用（一次 HTTP 尝试）的往返耗时
    success        INTEGER NOT NULL,           -- 1 成功 / 0 失败（重试的每次尝试各记一行）
    error          TEXT,                       -- 失败原因（成功为 NULL，按长度截断）
    chain_position INTEGER,                    -- 在 fallback 链中的 1 起名次；直接调用为 1
    attempt        INTEGER NOT NULL DEFAULT 1, -- 该 config 内的第几次尝试（1 起）
    created_at     TEXT NOT NULL               -- 调用时刻（UTC RFC 3339 定宽串）
);

-- 读取路径只有两种，都按时间范围：
--   1) 窗口聚合（`created_at >= ?`，全部 provider 一次扫出，provider 是折叠维度
--      而非过滤条件）；
--   2) 保留期清理（`created_at < ?`）。
-- provider 作为复合索引的第二列随行存放：窗口扫描用其前缀，将来若需要
-- 「某 provider 的窗口」查询，同一个索引即可服务，无需再加一条。
CREATE INDEX idx_llm_call_samples_window ON llm_call_samples (created_at, provider);
