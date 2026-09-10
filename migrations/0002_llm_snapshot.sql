-- 0.10.2 (RENG-38)：评审历史的 LLM 使用快照。
-- 报告侧冗余名称快照（不做外键、不做软删）：llm_providers 是整表
-- DELETE+INSERT 语义，行 id 每次保存都会重生成，外键引用必然悬空；
-- 历史展示要的是"当时实际用了哪个 provider/model"，存名称快照即可。
-- 方言约束与 0001 一致：占位符 `?`（由 store 层重写）、JSON 一律 TEXT。
-- 旧行三列均为 NULL，前端显示「未知」/不显示，不做回填。

-- 每条专家报告实际使用的 LLM（命中 fallback 链中第几个 config 就记哪个）。
ALTER TABLE expert_reports ADD COLUMN llm_provider TEXT;
ALTER TABLE expert_reports ADD COLUMN llm_model TEXT;

-- 评审级去重后的 provider/model 对列表（JSON 数组 TEXT，
-- 形如 [{"provider":"xiaomi","model":"mimo-v2.5-pro"}]），
-- 供历史列表页免解析 reviews.result 直接展示。
ALTER TABLE reviews ADD COLUMN llm_summary TEXT;
