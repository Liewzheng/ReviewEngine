-- RENG-78：通信探测样本（llm_probe_samples）。
-- 目的：LLM 状态页要显示的是**通信延迟** —— 探针本身（`GET {api_base}/models`
-- 的一次往返，DNS + TCP + TLS + HTTP）的历史均值，与模型生成过程无关。
-- 此前每次探测只保留最新一个值（`lastProbeLatencyMs`，RENG-36），没有样本、
-- 没有均值；0004 的 llm_call_samples 记的是评审调用耗时（含生成），0006 的
-- ttfb_ms 在非流式请求下 ≈ latency_ms（RENG-77 实测结论），都不是用户要的
-- 「纯网络往返」。本表为每次探测落一行，作为该均值的唯一数据来源。
--
-- 方言约束与 0001–0006 一致：
--   * 时间戳一律 TEXT（Rust 侧 chrono 生成的定宽 RFC 3339 UTC 串，
--     字典序 == 时间序，范围过滤与 ORDER BY 语义不变）；
--   * 布尔一律 INTEGER 0/1（Any 驱动不认 SQLite 的 BOOLEAN 声明类型）；
--   * 不使用 RETURNING（主键 Rust 侧生成 UUID v4）。
--
-- entry_fp 与 0005 给 llm_call_samples 加的那一列同源：卡片身份是
-- (provider, api_base, model, api_key) 的四元组指纹，provider 名只是展示标签
-- （RENG-75，两张卡可以同名）。探测延迟是**这张卡**的 api_base 上的网络往返，
-- 按 provider 名聚合会把一张卡的链路算到另一张卡头上，所以按指纹折叠。
-- 与 0005 的区别：0005 是给已有表补列，旧行只能是 NULL（读取侧有「未标记桶」
-- 升级规则）；本表随列一起出生，写入侧每次都能给出指纹，故声明 NOT NULL，
-- 不需要未标记桶。指纹由 crate::llm::identity::entry_fp 生成，服务端专用、
-- 绝不出现在响应里（同 0005）。
--
-- 幂等：重复写入是允许的（一次探测一行），不需要唯一键。

-- ── 逐次连通性探测采样 ──
CREATE TABLE llm_probe_samples (
    id         TEXT PRIMARY KEY,           -- UUID v4，Rust 侧生成（代理键）
    provider   TEXT NOT NULL,              -- 探测的 config.provider（展示标签，可重复）
    entry_fp   TEXT NOT NULL,              -- 被探测卡片的四元组指纹（RENG-75）
    latency_ms INTEGER,                    -- 探测往返耗时；**失败为 NULL**
                                           -- （一次 401 可能是 5ms、一次超时是 120s：
                                           --  那是失败的形态，不是链路快慢，见
                                           --  src/server/api/llm_probe.rs 的均值口径）
    success    INTEGER NOT NULL,           -- 1 成功 / 0 失败
    error      TEXT,                       -- 失败原因（成功为 NULL，按长度截断）
    created_at TEXT NOT NULL               -- 探测时刻（UTC RFC 3339 定宽串）
);

-- 读取路径只有两种，都按时间范围：
--   1) 窗口聚合（`created_at >= ?`，一次扫出后在 Rust 侧按指纹折叠）；
--   2) 保留期清理（`created_at < ?`）。
-- provider 作为第二列随行存放：窗口扫描用它做投影，将来若需要「某 provider 的
-- 窗口」查询，同一个索引即可服务（与 0004 的理由一致）。
CREATE INDEX idx_llm_probe_samples_window ON llm_probe_samples (created_at, provider);
