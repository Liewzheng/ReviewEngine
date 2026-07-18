---
title: 统一评分系统架构设计
description: 将 MR review 评分与 repo-review 评分合并为一套统一的 Scorable trait + weighted 函数
tags:
  - architecture
  - scoring
  - refactoring
related:
  - ../../src/scoring/mod.rs
  - ../../src/repo/experts/mod.rs
  - ../../src/models/mod.rs
  - ../../src/tools/repo_review.rs
  - ../../src/output/team_renderer.rs
  - ../../src/team/lead_consolidator.rs
---

# 统一评分系统架构设计

## 状态更新（v0.7.10）

> 本节为实施后的状态记录；下文为原始设计，保留作历史决策记录。

实际落地的是**函数级复用**方案，而非本文的 `Scorable` trait 方案：

- repo 侧 `weighted_total()` 复用 `scoring::review::compute_weighted()`（`src/repo/experts/mod.rs:236`）
- findings 统一走 `src/team/lead_consolidator.rs` 合并过滤
- `src/scoring/repo.rs` 已删除，scoring 模块收敛为 `mod.rs` + `review.rs`
- MR 主路径已接入 Lead 聚合与总分（v0.7.10 的 A1 项，见 `CHANGELOG.md`）：`run_experts` 返回 `ConsolidatedReport`，报告渲染 Lead Summary（Overall Score / TL;DR / 冲突）

剩余差距：

- ~~repo 侧仍保留独立的 `score_to_risk_level()`（`src/repo/experts/mod.rs:118`，返回 `&str`），未与 `scoring::review` 的 `RiskLevel` 映射统一~~ **已解决（v0.7.11）**：
  - `RiskLevel` 新增 `Healthy` 变体（最高档，score > `healthy_min`，默认 90，即 91+）
  - `RiskThresholdConfig` 新增 `healthy_min`（默认 90）；`score_to_risk_level_with_config()` 先判 healthy 档，再按原 5 档映射，既有阈值 20/40/60/80 不变
  - repo 侧 `score_to_risk_level()` 已删除，`weighted_total()` 与报告构建统一走 `scoring::review::score_to_risk_level_with_config()`（默认阈值 40/60/80/90，与旧 repo 分段一致，仅 81–90 档标签由 `low` 更名为 `low-medium`）
  - `RepoReviewOutput` 的 `risk_level` / `risk_label` 字段改为 `RiskLevel` 枚举，经 `models::risk_level_lowercase` serde 适配器序列化为小写字符串（如 `"healthy"` / `"medium"`），JSON 形式与旧输出保持一致

> 注意：下文「文件变更清单」与「迁移对照表」描述的是未被采纳的原始方案（`Scorable` trait 路线），仅作历史记录，请勿当作当前实现状态。

---

## 背景

当前项目存在**三套独立的评分系统**，彼此不共享类型或函数：

| 系统 | 位置 | 核心函数 | 评分方式 | 使用状态 |
|------|------|---------|---------|---------|
| MR review 评分 | `src/scoring/mod.rs` | `weighted_overall_score()`, `expert_score()`, `score_to_risk_level()` | 按 finding severity 扣分，加权汇总 | 已实现但**未接入**活跃路径 |
| repo-review 评分 | `src/repo/experts/mod.rs` | `weighted_total()` | 专家输出 `ExpertScore`，加权汇总 | **已接入** `repo-review` CLI |
| repo 硬编码评分 | `src/repo/scoring.rs` | `score_repository()` | 大文件/安全/生成文件线性扣分 | **已废弃**（被 expert 系统替代）|

### 核心问题

1. **三个 `weighted_score` 函数做同一件事**（Σ score × weight / Σ weight），类型不同
2. **两个 `score_to_risk_level` 映射**，边界不同、返回类型不同（enum vs string）
3. **`scoring` 模块的消费者**（`team_renderer`, `lead_consolidator`）定义了完整流程但未接入活跃路径
4. **新增评分维度需要改多处代码**，没有统一的扩展点

---

## 设计目标

1. **统一**：一套 `weighted()` 函数、一个 `RiskLevel` 枚举、一种分数结构
2. **可扩展**：新增评分维度只需实现 `Scorable` trait，不改 `weighted()`
3. **向后兼容**：现有 `repo-review` CLI 输出不变，MR review 路径逐步迁移
4. **最小改动**：不改变 `ExpertScore` 的字段结构，只加新 trait 实现

---

## 核心抽象

### `Scorable` trait

```rust
/// Anything that can contribute a scored dimension.
pub trait Scorable {
    fn name(&self) -> &str;
    fn score(&self) -> u8;    // 0-100
    fn weight(&self) -> u8;   // 0-100
}
```

### `Score` 结构体

```rust
/// A unified scoring result from any review context.
pub struct Score {
    pub value: u8,              // 0-100
    pub risk_level: RiskLevel,
    pub dimensions: Vec<DimensionRecord>,
}

/// A single dimension's contribution to the overall score.
pub struct DimensionRecord {
    pub name: String,
    pub score: u8,
    pub weight: u8,
    pub summary: String,
}
```

### `RiskLevel` 枚举（扩展）

在现有 `RiskLevel` 中新增 `Healthy` 变体，合并 repo-review 的风险映射：

```rust
pub enum RiskLevel {
    Healthy,    // 91-100  ← 新增
    Low,        // 81-90
    Medium,     // 61-80
    High,       // 41-60
    Critical,   // 0-40
}
```

### `weighted()` 函数

```rust
/// Compute weighted score from any set of Scorable items.
pub fn weighted(items: &[impl Scorable]) -> Score {
    let total_weight: u32 = items.iter().map(|d| d.weight() as u32).sum();
    if total_weight == 0 {
        return Score { value: 0, risk_level: RiskLevel::Critical, dimensions: vec![] };
    }
    let numerator: f64 = items.iter()
        .map(|d| d.score() as f64 * d.weight() as f64)
        .sum();
    let value = (numerator / total_weight as f64).round().clamp(0.0, 100.0) as u8;
    let risk_level = score_to_risk_level(value);
    let dimensions = items.iter().map(|d| DimensionRecord {
        name: d.name().to_string(),
        score: d.score(),
        weight: d.weight(),
        summary: String::new(),
    }).collect();
    Score { value, risk_level, dimensions }
}
```

---

## 谁实现 `Scorable`

### `ExpertScore`（repo-review 专家）

```rust
// src/repo/experts/mod.rs
impl Scorable for ExpertScore {
    fn name(&self) -> &str { &self.expert_name }
    fn score(&self) -> u8 { self.score }
    fn weight(&self) -> u8 { self.weight }
}
```

现有 `weighted_total()` 改为调用 `scoring::weighted()`：

```rust
pub fn weighted_total(scores: &[ExpertScore]) -> (u8, String) {
    let result = scoring::weighted(scores);
    (result.value, result.risk_level.to_string())
}
```

### MR Expert Record（新 type）

```rust
// src/scoring/mod.rs
pub struct ExpertRecord {
    pub name: String,
    pub findings: Vec<Finding>,
    pub weight: u8,
}

impl Scorable for ExpertRecord {
    fn name(&self) -> &str { &self.name }
    fn score(&self) -> u8 { expert_score(&self.findings) }
    fn weight(&self) -> u8 { self.weight }
}
```

`compute_overall()` 改为：

```rust
pub fn compute_overall(data: &[(&str, &[Finding], u8)]) -> (u8, RiskLevel) {
    let records: Vec<ExpertRecord> = data.iter()
        .map(|(name, findings, weight)| ExpertRecord {
            name: name.to_string(),
            findings: findings.to_vec(),
            weight: *weight,
        })
        .collect();
    let result = weighted(&records);
    (result.value, result.risk_level)
}
```

---

## 数据流

```
┌─ MR Review ───────────────────┐
│  ExpertRecord { name,         │
│    findings, weight }         │──┐
│  impl Scorable               │  │
└───────────────────────────────┘  │
                                  ├─→ scoring::weighted(&items)
┌─ Repo Review ─────────────────┐  │      ↓
│  ExpertScore { name, score,   │──┘   Score { value, risk_level,
│    weight, summary, details } │      dimensions: [{
│  impl Scorable               │          name, score, weight
└───────────────────────────────┘      }]}
```

---

## 文件变更清单

| 文件 | 操作 | 行数 |
|------|------|------|
| `src/models/mod.rs` | `RiskLevel` 新增 `Healthy` 变体 + `Display` 更新 | +3 |
| `src/scoring/mod.rs` | 新增 `Scorable` trait、`Score`、`DimensionRecord`、`ExpertRecord`、`weighted()`；保留 `expert_score()`、`score_to_risk_level()`；删除 `weighted_overall_score()`、`compute_overall()`；更新测试 | +50/-15 |
| `src/repo/experts/mod.rs` | `ExpertScore` impl `Scorable`；`weighted_total()` 改为调用 `scoring::weighted()`；风险映射改为 `RiskLevel` | +10/-10 |
| `src/tools/repo_review.rs` | `build_output()` 改用 `Score`；`ExpertScoreOutput` 保留（序列化用）| +5/-5 |
| `src/output/team_renderer.rs` | 改用 `Scorable` trait | +3/-3 |
| `src/team/lead_consolidator.rs` | 改用 `Score` | +3/-3 |
| **合计** | | **~74** |

---

## 删除的旧代码

| 当前函数 | 替换 |
|---------|------|
| `scoring::weighted_overall_score()` | `scoring::weighted()` |
| `scoring::compute_overall()` | `scoring::weighted()` + `ExpertRecord` |
| `repo/experts::weighted_total()` | `scoring::weighted()`（包装调用）|
| `repo/experts` 中的 string 风险映射（`match score { 0..=40 => "critical" ... }`）| `RiskLevel::to_string()` |

不删除但不再导出的：

| 项目 | 保留理由 |
|------|---------|
| `scoring::expert_score()` | 仍用于计算 findings-based 专家分 |
| `scoring::score_to_risk_level()` | 仍用于单一值映射 |
| `scoring::ReviewScoreRecord` | 保留避免破坏测试，标记 deprecated |

---

## 验收标准

1. `cargo test` 全部通过（测试数不变）
2. `cargo clippy` 零新 warning
3. `review-engine repo-review --local-path .` 输出格式不变（score/risk_level 字段值完全一致）
4. `scoring::weighted(&[ExpertRecord])` 与旧 `weighted_overall_score` 结果一致
5. `scoring::weighted(&[ExpertScore])` 与旧 `weighted_total` 结果一致

---

## 后续扩展方式

新加评分维度只需：

```rust
struct MyExpert { score: u8, weight: u8 }
impl Scorable for MyExpert { /* 3 个方法 */ }

let results = scoring::weighted(&[my_expert, other_expert]);
```

无需修改 `weighted()` 函数，无需新增 `mod.rs` 以外的文件。

---

## 附录 A：大文件阶梯减分算法

`code_organization` 专家在评估大文件时使用 **阶梯式超出量减分**，而非一刀切的阈值计数。

### 算法

```rust
// 每个文件的超出量 = max(LOC - 500, 0)
// 总超出量 = Σ 所有大文件的超出量（仅源码，排除 Doc/Config）
// 扣分 = (总超出量 / 100).min(40)
```

| 参数 | 值 | 说明 |
|------|-----|------|
| 基线 | 500 LOC | 低于此不扣分 |
| 阶梯步长 | 100 LOC | 每超 100 行扣 1 分 |
| 扣分上限 | 40 | 最多扣 40 分 |

### 对比

| 文件 | LOC | 超出量 | 旧方案（平权） | 新方案（阶梯） |
|------|-----|--------|-------------|-------------|
| `processor.rs` (含测试) | 672 | 172 | 3 分 | 1.7 分 |
| `resolver.rs` (含测试) | 624 | 124 | 3 分 | 1.2 分 |
| `client.rs` | 602 | 102 | 3 分 | 1.0 分 |
| 其他 <600 LOC 的文件 | 532-537 | 32-37 | 各 3 分 | 各 0.3-0.4 分 |
| **6 个大文件合计** | | **~504** | **18 分** | **~5 分** |

### 设计理由

1. **削峰填谷**：500 行文件只是略大，1000 行文件是真正需要关注的——两者不应扣相同分数
2. **区分测试增长**：测试代码合理增加不会大幅拉低分数（如 `processor.rs` 因加测试从 385→672，超出 172 只扣 1.7 分，而不是旧方案的 3 分 × 文件数）
3. **保持上限**：无论多少个大文件，该项最多扣 40 分（与旧方案一致）
4. **无阈值跳跃**：旧方案有 `if large_count > 5` 的硬门槛，1-5 个大文件完全不扣分，第 6 个突然扣 18 分——阶梯算法消除了这种不连续
