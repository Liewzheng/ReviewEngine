-- 0.10.28 (RENG-75)：provider 卡片身份 = 四元组指纹 hash(provider, api_base, model, api_key)。
-- provider 名降级为展示标签（可重复），统计按指纹聚合，因此逐次调用样本必须
-- 记录它命中的是哪一张卡。指纹是可空列：0005 之前写入的旧行保持 NULL（聚合时
-- 落入「未标记」桶，由 API 层按 (provider, model) 唯一启用卡规则决定是否并入）。
--
-- 方言约束与 0001–0004 一致（TEXT 列、无 RETURNING、可空无默认值 → SQLite 与
-- PostgreSQL 的同一条 ALTER 均可重放；迁移幂等性由 _sqlx_migrations 台账保证）。
--
-- 不新增索引：本表的全部读取仍是「窗口扫描后在 Rust 里折叠」（见 0004 的说明与
-- src/server/api/llm_latency.rs），entry_fp 是折叠维度而不是过滤条件，0004 的
-- (created_at, provider) 索引已经界定了扫描范围；再加一条 (created_at, entry_fp)
-- 与现有索引服务同一个 range scan，只会白白增加每次写入的维护成本。

ALTER TABLE llm_call_samples ADD COLUMN entry_fp TEXT;

-- 同一语义包的另一半：0001 给 llm_providers.provider 建的唯一索引
-- （idx_llm_providers_provider）与「provider 名只是展示标签、可重复」直接冲突 ——
-- 两张同名卡（同服务的两个账户、或一个账户两个模型）落库时第二条 INSERT 会撞
-- 唯一约束，PUT /config 直接 500「insert llm_provider …」。身份既然已改成
-- (provider, api_base, model, api_key) 四元组指纹，名字唯一性就不再是正确的不变量，
-- 必须去掉这条索引：否则「复制卡片」在 UI 上根本存不下来。
--
-- 行身份不受影响：主键仍是 id（UUID v4）；列表顺序仍是 raw.position（RENG-55），
-- 与 provider 名无关。0001 的 CREATE UNIQUE INDEX 保持原样（已发布的迁移不改写），
-- 由这里 DROP 掉 —— 对全新库与既有库都得到同一最终 schema。
DROP INDEX IF EXISTS idx_llm_providers_provider;
