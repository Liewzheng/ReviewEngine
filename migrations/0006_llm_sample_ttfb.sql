-- 0.10.30 (RENG-77)：把「通信延迟」从「总耗时」里分出来。
-- 目的：LLM 页「平均延迟」KPI 与卡片首个统计量此前显示 latency_ms，即一次
-- HTTP 请求的**全量耗时**（含模型生成，实测 17s 量级）。用户要的是**通信
-- 延迟**（请求发出到收到响应头/首字节的往返），因此每次尝试额外记录 ttfb_ms。
--
-- 可空列：0006 之前写入的旧行保持 NULL。聚合时 NULL **不参与**均值计算（也
-- 不当作 0）——「不知道」与「测得 0ms」是两件事，同 RENG-56/57 的既有约定。
-- 新的写入路径每次都写两列（失败尝试也可能没有 ttfb：连接层失败根本没有
-- 首字节，此时为 NULL）。
--
-- 方言约束与 0001–0005 一致（TEXT/INTEGER 列、无 RETURNING、可空无默认值 →
-- SQLite 与 PostgreSQL 的同一条 ALTER 均可重放；幂等性由 _sqlx_migrations
-- 台账保证）。
--
-- 不新增索引：读取路径不变（仍是一次 (created_at, provider) 窗口扫描后在
-- Rust 侧按指纹折叠，见 0004/0005 的说明与 src/server/api/llm_latency.rs），
-- ttfb_ms 是投影列而不是过滤条件。

ALTER TABLE llm_call_samples ADD COLUMN ttfb_ms INTEGER;
