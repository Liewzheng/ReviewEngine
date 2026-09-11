-- 0.10.6 (RENG-43)：MR/PR 参与者的讨论作者元数据。
-- 目的：历史「参与者」列表要能在不重新请求 provider API 的情况下，
-- 从 mr_discussions 还原评论者（含头像与 robot 判定）。
-- 方言约束与 0001/0002 一致：
--   * author_id 声明为 TEXT：参与者 id 来自 provider（u64），TEXT 列在
--     SQLite 与 PostgreSQL 上都接受同一个 Rust String 绑定；用 BIGINT
--     则 SQLite 侧（sqlx Any 驱动的声明类型白名单）读回不稳。
--   * 布尔一律 INTEGER 0/1（0001 §3.1），故 author_bot 用 INTEGER。
--   * 旧行三列分别为 NULL / NULL / 0，前端按「无头像 / 非 bot」降级，不回填。

ALTER TABLE mr_discussions ADD COLUMN author_id TEXT;
ALTER TABLE mr_discussions ADD COLUMN author_avatar_url TEXT;
ALTER TABLE mr_discussions ADD COLUMN author_bot INTEGER NOT NULL DEFAULT 0;
