---
title: review_engine 虚拟研发团队专业化设计
description: CodeReview Board 的专业团队设计理念、角色标准、审核流程、输出规范与评分体系
tags:
  - review-engine
  - product-design
  - virtual-team
  - scoring
related:
  - code-audit-default.toml
  - ../.notes/review_engine_rs_roadmap.md
  - repo_aware_review_strategy.md
---
> **Status:** Design proposal — future phases described here may not be fully implemented yet.
>
> **状态（v0.7.10）**：多专家团队（TOML 定义角色/权重/trigger/commands）与 Lead 汇总（`lead_consolidator` 合并去重）已落地。进行中：A5（评分公式统一）、A7（冲突呈现）；关联项 A2（仓库感知 prompt 注入）亦在推进，MR 主路径评分接入（A1）尚未完成。
>
> 视角：产品设计师。
> 目标：让 AI 审核团队不仅“人多”，更要“专业”——像一支真实、可信、高效的 senior engineering team 那样工作。

---

## 一、核心设计原则

### 1.1 从“多专家并行”到“专业团队协作”

当前很多 AI review 工具的问题是：多个专家各说各话，输出像一堆零散评论。

真正的专业团队应该：

- **有明确分工**：每个人知道自己负责什么、不负责什么
- **有协作流程**：Lead 统筹，专家互补，冲突有处理机制
- **有输出标准**：每条意见都有依据、有分级、有行动建议
- **有质量把控**：低质量意见被过滤，重复意见被合并
- **有专业形象**：可信、直接、建设性，不夸张、不人身攻击

### 1.2 专业化团队的五个标志

| 标志 | 表现 |
|------|------|
| **身份清晰** | 每个 reviewer 有名字、角色、专业领域、发言风格 |
| **流程规范** | 准备 → 独立审查 → 交叉校验 → Lead 汇总 → 输出报告 |
| **输出专业** | 每条 finding 有证据、severity、confidence、action |
| **可配置** | 专家数量、角色、权重、prompt 全部由 TOML 定义，review-engine 只提供通用编排接口 |
| **持续进化** | 从反馈中学习，校准判断标准 |

---

## 二、团队构成与角色设计

### 2.1 团队命名

建议品牌名：**CodeReview Board** 或 **Engineering Review Panel（ERP）**

每次 review 时展示：

> *"This PR was reviewed by the Engineering Review Panel: Alex (Staff Engineer), Sam (Security Lead), Jordan (Performance Engineer), Taylor (QA Lead), Riley (Refactoring Lead), and Drew (Docs Lead)."*

### 2.2 标准角色矩阵（默认配置示例）

**重要：以下角色不是写死的，而是 `code-audit-default.toml` 中的默认配置。** 每个专家都像 Class 一样定义，拥有自己的字段和能力；每个团队都可以在自己的 `.code-audit-config.toml` 中（当前仍兼容 `.pr-agent.toml`，但后者将在未来版本中移除）：

- 启用/禁用任意专家
- 新增自定义专家（如 Database Lead、DevOps Lead、Compliance Lead）
- 调整 `weight`、`focus`、`principles`、`model`
- 修改 `trigger` 让专家按文件类型/路径按需参与
- 配置每个专家参与哪些命令（`commands`），如 `/review`、`/describe`、`/improve`、`/ask`、`/repo-review`

**命令开关**：`[commands]` 下所有命令默认关闭，用户必须显式启用。

| 角色 | 英文名 | 职责 | 不做什么 |
|------|--------|------|---------|
| **审核负责人** | Lead Reviewer | 统筹全局、把控质量、汇总结论、处理冲突 | 不代替其他专家做深度领域判断 |
| **安全负责人** | Security Lead | 注入、越权、密钥、输入验证、加密 | 不评论性能或代码风格 |
| **性能负责人** | Performance Lead | 算法、内存、并发、IO、资源泄漏 | 不评论业务逻辑正确性 |
| **架构负责人** | Architecture Lead | 模块耦合、接口设计、可扩展性、SOLID | 不抓拼写或格式化 |
| **质量负责人** | Quality Lead | 测试覆盖、边界条件、回归风险 | 不替代安全专家 |
| **复用负责人** | Reuse Lead | 重复代码、抽象缺失、大函数/God Class | 不评论架构方向 |
| **文档负责人** | Docs Lead | 文档一致性、README、CHANGELOG、项目目标对齐 | 不评论代码实现细节 |
| **API 负责人** | API Lead（可选） | 接口契约、向后兼容、OpenAPI | 只在 API 相关变更时参与 |
| **数据负责人** | Data Lead（可选） | DB schema、查询、迁移、索引 | 只在数据层变更时参与 |

### 2.3 每个角色的完整画像

不只是 `name` + `role`，而是一个完整的专业画像：

```toml
[commands]
review = true
repo_review = true

[review_experts.security]
name = "Sam"
role = "Security Lead"
title = "Staff Security Engineer"
enabled = true
weight = 25
commands = ["review", "repo_review"]
style = "precise, references standards, focuses on exploitable scenarios"
principles = [
    "Only flag issues I can explain with a concrete attack scenario",
    "Cite OWASP/CWE when relevant",
    "Distinguish between 'definitely vulnerable' and 'worth verifying'",
]
focus = ["injection", "authz", "secrets", "input_validation", "crypto"]
standards = ["OWASP Top 10", "CWE", "ISO 27001"]
```

### 2.4 专家即 Class：命令与能力模型

每个专家都像 **Class** 一样定义，拥有字段（属性）和 `commands`（可参与的行为）。

```toml
[commands]
review = true
improve = false

[review_experts.performance]
enabled = true
weight = 15
commands = ["review", "improve"]
# ... 其他字段即该 expert 的属性和 prompt
```

**行为模型**：

| 层级 | 作用 | 默认值 |
|------|------|--------|
| `[commands]` | 全局开关，决定哪些命令可用 | 全部 `false` |
| `review_experts.<name>.enabled` | 该专家是否被实例化 | 默认配置中 `true` |
| `review_experts.<name>.commands` | 该专家参与哪些命令 | 按角色预分配 |

**运行时规则**：

```rust
pub fn select_experts_for_command(
    command: &str,
    experts: &[ExpertDef],
    commands_config: &HashMap<String, bool>,
) -> Vec<ExpertDef> {
    if !commands_config.get(command).unwrap_or(&false) {
        return vec![]; // 命令未启用
    }

    experts
        .iter()
        .filter(|e| e.enabled && e.commands.contains(&command.to_string()))
        .cloned()
        .collect()
}
```

这意味着：
- 即使 expert `enabled = true`，如果其 `commands` 不包含当前命令，也不会被调用
- 即使 expert 支持某命令，如果 `[commands]` 中该命令为 `false`，也不会被调用
- 用户可以精确控制 "哪个命令由哪些专家执行"

---

## 三、专业审核流程

### 3.1 五阶段流程

```
Phase 1: Briefing（任务简报）
  - Lead 接收 MR，分配上下文，识别风险区域
  - 决定哪些专家参与（Core Team / Dynamic Team）

Phase 2: Independent Review（独立审查）
  - 每位专家独立审阅自己负责的范围
  - 使用 RepoBrowser 查阅仓库上下文
  - 输出带 confidence 的 finding

Phase 3: Cross-check（交叉校验）
  - 专家之间可以看到彼此的 finding
  - 对重复发现达成共识或补充
  - 对冲突发现标注不同观点

Phase 4: Lead Consolidation（负责人汇总）
  - Lead 审核所有 finding 的质量
  - 过滤低 confidence / 无依据的 finding
  - 生成 TL;DR 和整体评估

Phase 5: Report Delivery（报告交付）
  - 输出结构化团队报告
  - 每条 finding 标注提出者、角色、confidence
  - 提供 action items 和优先级
```

### 3.2 冲突处理机制

当专家意见冲突时，不隐藏，而是专业呈现：

```markdown
### ⚖️ Reviewer Discussion
- **Jordan (Performance)**: Caching this query would reduce DB load by ~90%.
- **Alex (Architecture)**: I disagree at this stage. The query is not on a hot path, and adding cache introduces invalidation complexity.
- **Sam (Security)**: If cached, ensure no sensitive data is stored without TTL and encryption.

**Lead resolution**: Defer caching. Add a comment with the query latency metric and revisit if it becomes a hotspot.
```

---

## 四、Finding 输出标准

### 4.1 每条 finding 必须包含

```rust
pub struct Finding {
    pub id: String,                    // 唯一 ID，便于追踪
    pub file: String,                  // 文件路径
    pub line: Option<u32>,             // 行号
    pub line_end: Option<u32>,         // 结束行号（范围）
    pub severity: Severity,            // critical / high / medium / low / note
    pub confidence: u8,                // 1-10
    pub category: String,              // 分类，如 "sql_injection"
    pub title: String,                 // 一句话标题
    pub summary: String,               // 问题描述
    pub evidence: String,              // 代码片段或具体证据
    pub impact: String,                // 不修复会有什么后果
    pub recommendation: String,        // 具体修复建议
    pub effort: Effort,                // trivial / small / medium / large
    pub expert_name: String,           // 提出者名字
    pub expert_role: String,           // 提出者角色
    pub agrees_with: Vec<String>,      // 同意的其他专家
    pub references: Vec<String>,       // 参考标准或文档
}
```

### 4.2 Severity 定义

| 级别 | 定义 | 示例 |
|------|------|------|
| **Critical** | 必须立即修复，否则可能生产事故、数据丢失、安全漏洞 | SQL 注入、未授权访问、数据竞争 |
| **High** | 显著缺陷，很可能导致 bug 或重大技术债 | N+1 查询、循环依赖、缺失关键测试 |
| **Medium** | 值得关注，但影响可控 | 重复代码、过大函数、部分边界未处理 |
| **Low** | 建议性改进 | 命名可更清晰、注释可更完整 |
| **Note** | 信息性提示，不需要行动 | 设计决策说明、相关上下文 |

### 4.3 Confidence 定义

| 分数 | 含义 |
|------|------|
| 9-10 | 确信，有明确证据 |
| 7-8 | 很可能，需要作者确认 |
| 5-6 | 存疑，建议关注 |
| <5 | 不报告 |

### 4.4 Effort 定义

| 级别 | 说明 |
|------|------|
| **trivial** | 几分钟，如重命名、加个常量 |
| **small** | 1-4 小时 |
| **medium** | 半天-2 天 |
| **large** | 2 天以上，需要专门计划 |

---

## 五、团队沟通风格指南

### 5.1 专业语气特征

- **直接**："This function has an N+1 query pattern."
- **具体**：给出代码片段、行号、场景。
- **建设性**：每个问题配建议。
- **谦逊**：不确定时明确说 "I cannot confirm from the diff alone, but..."
- **无情绪化**：不用 "垃圾"、"屎山" 等词，用 "high technical debt"、"significant duplication"。

### 5.2 避免的语言

| ❌ 不专业 | ✅ 专业 |
|----------|--------|
| "这段代码太烂了" | "This code introduces significant duplication that will increase maintenance cost." |
| "你怎么能这么写" | "This pattern may lead to race conditions under concurrent load." |
| "必须重写" | "I recommend refactoring this into a dedicated service; estimated effort: medium." |
| "肯定有 bug" | "There is a high-confidence risk of null pointer dereference when..." |

### 5.3 Riley（复用专家）的专业化改造

原 prompt 里的 "看到重复就骂" 可以保留犀利感，但要升级：

```markdown
## 你的身份
Riley，Staff Software Engineer，Refactoring Lead。
你关注代码的可维护性、可测试性和长期演进成本。

## 审查原则
1. **DRY**：相同逻辑出现 2 次以上，建议提取；出现 4 次以上，必须提取。
2. **单一职责**：函数超过 50 行或做 2 件以上不同的事，建议拆分。
3. **避免魔法值**：硬编码值在 2 处以上出现，建议常量化或配置化。
4. **警惕 God Class**：一个类/模块承担 3 个以上独立职责，建议拆分。

## 输出标准
每条 finding 必须说明：
- 重复/问题的具体位置
- 影响范围（几个文件、几处出现）
- 维护成本或 bug 风险
- 重构建议及估算 effort
- severity 和 confidence

## 语气
直接、具体、基于事实。用数据说话，不用情绪词。
```

### 5.4 Drew（文档专家）的专业化设计

文档专家不是拼写检查器，而是**一致性守门人**：

```markdown
## 你的身份
Drew，Staff Technical Writer，Docs Lead。
你关注代码变更与文档、项目目标、对外承诺之间的一致性。

## 审查原则
1. **行为变更必须同步文档**：新增/修改功能时，README、API 文档、用户指南必须同步更新。
2. **CHANGELOG 必须可追踪**：每次 user-visible 变更都应有对应的 CHANGELOG 条目。
3. **PR 描述必须反映 diff**：标题和描述应准确概括变更范围，不能遗漏关键改动。
4. **项目目标一致性**：变更是否与 README 中声明的项目目标、架构方向一致。
5. **弃用和破坏性变更必须显式说明**：不能悄无声息地破坏向后兼容。

## 输出标准
每条 docs finding 必须说明：
- 哪个文档/描述与代码不一致
- 缺少什么文档或变更条目
- 为什么会影响用户或维护者
- 建议补充的具体内容或位置
- severity 和 confidence

## 语气
清晰、具体、以用户和维护者为中心。不抓拼写，只抓不一致和遗漏。
```

---

## 六、质量把控机制

### 6.1 Lead 作为质量守门人

Lead Reviewer 不只是汇总，还要做质量检查：

- 删除 confidence < 6 的 finding（或降级到 Note）
- 合并重复 finding
- 要求专家补充证据（如果 finding 缺少代码片段）
- 标记冲突并给出 resolution
- 确保每条 high/critical 都有明确 action

### 6.2 专家自审（Self-reflection）

每个专家输出 finding 后，可选做一次自审：

> "Review your own findings. Remove any that are speculative, stylistic, or lack concrete evidence. Keep only actionable, evidence-based issues."

### 6.3 用户反馈闭环

```rust
pub struct FindingFeedback {
    pub finding_id: String,
    pub was_helpful: bool,
    pub was_fixed: bool,
    pub user_comment: Option<String>,
}
```

- 统计每个 expert 的 helpful rate
- 统计每个 category 的误报率
- 定期校准 prompt 和判断标准

---

## 七、报告呈现设计

### 7.1 报告头部

```markdown
## Engineering Review Panel — N reviewers · 14 seconds（默认配置下为 6 人）

**Overall Assessment**: ✅ Looks good, 2 medium issues to address
**Risk Level**: Low-Medium
**Confidence**: High

**Reviewers**: Alex (Lead), Sam (Security), Jordan (Performance), Taylor (Quality), Riley (Reuse), Drew (Docs)

### TL;DR
- 2 medium-severity issues: missing test coverage for edge case, duplicated validation logic.
- No security or performance blockers.
- Estimated fix effort: small.
```

### 7.2 Finding 卡片

```markdown
### 🔴 Critical · Confidence 9/10 · Sam (Security Lead)

**SQL Injection Risk** in `src/db.rs:42-45`

**Evidence**:
```python
cursor.execute(f"SELECT * FROM users WHERE id = {user_id}")
```

**Impact**: An attacker could manipulate `user_id` to execute arbitrary SQL.

**Recommendation**: Use parameterized queries.
```python
cursor.execute("SELECT * FROM users WHERE id = ?", (user_id,))
```

**Effort**: trivial
**References**: [OWASP SQL Injection](https://owasp.org/...)
```

### 7.3 冲突呈现

```markdown
### ⚖️ Reviewer Discussion
- **Jordan** suggests adding cache.
- **Alex** disagrees, citing added complexity.
- **Lead resolution**: Defer; monitor latency first.
```

---

## 八、专家权重与评分体系

### 8.1 设计目标

让团队报告不仅呈现 "谁发现了什么问题"，还要呈现 **"团队整体判断有多严重"** 和 **"每位专家在本次 review 中的贡献度/评分"**。

评分体系作用：
- **量化风险**：用 0-100 的分数让开发者一眼看清当前 MR/仓库健康度
- **体现专业分工**：不同专家权重不同，安全/架构问题对总分影响更大
- **驱动改进**：长期追踪每个专家评分的命中率、误报率，持续校准

### 8.2 评分体系的三个层次

```
┌─────────────────────────────────────────┐
│  Layer 1: Individual Expert Score       │
│  每位专家基于自己 findings 给出 0-100 分  │
├─────────────────────────────────────────┤
│  Layer 2: Weighted Overall Score        │
│  按专家权重加权平均，得到团队综合评分      │
├─────────────────────────────────────────┤
│  Layer 3: Risk Level & Trend            │
│  综合评分映射为 Risk Level，历史趋势追踪   │
└─────────────────────────────────────────┘
```

设计约束：
- **可解释**：每个分数都能说明是怎么算出来的
- **可配置**：权重、扣分规则可在 TOML 中调整
- **可校准**：根据用户反馈持续优化算法
- **不替代判断**：评分是辅助，Lead Reviewer 的最终评估仍起决定作用

### 8.3 专家权重（Weight）

每个专家在配置中有一个 `weight`（0-100），表示其对 **Overall Score / Risk Level** 的影响力。

```toml
[review_experts.lead]
enabled = true
weight = 25

[review_experts.security]
weight = 25

[review_experts.performance]
weight = 15

[review_experts.quality]
weight = 15

[review_experts.reuse]
weight = 20
```

权重分配原则：

| 专家 | 建议权重 | 理由 |
|------|---------|------|
| Lead | 20-30 | 全局判断，决定整体风险基调 |
| Security | 20-30 | 安全漏洞往往是硬性阻塞 |
| Reuse | 15-25 | 长期维护成本，反映技术债 |
| Performance | 10-20 | 性能问题影响体验，但通常有条件触发 |
| Quality | 10-20 | 测试/边界问题，影响回归风险 |

**权重校验**：系统在加载配置时严格校验启用专家的权重之和为 100。

```rust
pub fn validate_weights(experts: &[ExpertDef]) -> Result<()> {
    let enabled_experts: Vec<_> = experts.iter().filter(|e| e.enabled).collect();
    let total: u16 = enabled_experts.iter().map(|e| e.weight as u16).sum();

    if total != 100 {
        return Err(format!(
            "Expert weights must sum to 100 for enabled experts, got {}",
            total
        ));
    }
    Ok(())
}
```

### 8.4 专家个人评分（Expert Score）

对一次具体 review，每位专家输出一个 0-100 的评分，反映其视角下代码的健康度。基础分 100，按 finding 的严重程度和置信度扣分。

```rust
pub fn expert_score(findings: &[Finding]) -> u8 {
    let base = 100i32;

    let deductions: i32 = findings.iter().map(|f| {
        let severity_penalty = match f.severity.as_str() {
            "critical" => 25,
            "high" => 15,
            "medium" => 8,
            "low" => 3,
            "note" => 0,
            _ => 0,
        };

        let confidence_factor = f.confidence as f32 / 10.0;
        let consensus_multiplier = 1.0 + (f.agreeing_experts.len() as f32 * 0.2);

        (severity_penalty as f32 * confidence_factor * consensus_multiplier) as i32
    }).sum();

    (base - deductions.min(100)).max(0) as u8
}
```

参数说明：

| 参数 | 作用 |
|------|------|
| `severity_penalty` | 严重程度越高，扣分越多 |
| `confidence_factor` | confidence 越高，扣分越可信；confidence 低则打折 |
| `consensus_multiplier` | 多名专家共识的问题影响更大 |

评分解释：

| 分数区间 | 含义 | 颜色 |
|---------|------|------|
| 90-100 | 优秀，该维度几乎没有明显问题 | 🟢 |
| 70-89 | 良好，有少量可改进项 | 🟢 |
| 50-69 | 一般，存在需要关注的问题 | 🟡 |
| 30-49 | 较差，有 high 级别问题需要处理 | 🟠 |
| 0-29 | 很差，存在 critical 阻塞问题 | 🔴 |

### 8.5 加权综合评分（Weighted Overall Score）

```rust
pub fn weighted_overall_score(expert_scores: &[(String, u8, u8)]) -> u8 {
    // (expert_name, score, weight)
    let total_weight: u32 = expert_scores
        .iter()
        .map(|(_, _, weight)| *weight as u32)
        .sum();

    if total_weight == 0 {
        return 100;
    }

    let weighted_sum: u32 = expert_scores
        .iter()
        .map(|(_, score, weight)| (*score as u32) * (*weight as u32))
        .sum();

    (weighted_sum / total_weight) as u8
}
```

示例：

| 专家 | Score | Weight | Weighted Contribution |
|------|-------|--------|----------------------|
| Alex (Lead) | 75 | 25% | 18.75 |
| Sam (Security) | 85 | 25% | 21.25 |
| Jordan (Performance) | 60 | 15% | 9.00 |
| Taylor (Quality) | 55 | 15% | 8.25 |
| Riley (Reuse) | 45 | 20% | 9.00 |
| **Overall** | - | **100%** | **66.25 → 66/100** |

### 8.6 评分与 Risk Level 的映射

| Overall Score | Risk Level | 建议动作 |
|--------------|-----------|---------|
| 90-100 | Low | 可直接合并，或只处理 note/low |
| 70-89 | Low-Medium | 处理 medium 问题后合并 |
| 50-69 | Medium | 需要处理 high/medium 问题 |
| 30-49 | High | 存在明显缺陷，建议修复后再 review |
| 0-29 | Critical | 存在阻塞性问题，不建议合并 |

**Lead override**：算法映射的 Risk Level 可被 Lead Reviewer 覆盖。

```rust
pub struct OverallAssessment {
    pub algorithm_score: u8,
    pub algorithm_risk_level: RiskLevel,
    pub lead_risk_level: Option<RiskLevel>,
    pub lead_comment: Option<String>,
}
```

### 8.7 报告中的评分展示

```markdown
## CodeReview Board — N reviewers · 14 seconds（默认配置下为 6 人）

**Overall Score**: 66/100 (Medium)
**Risk Level**: Medium
**Overall Assessment**: Reuse and quality issues need attention before merge.

| Reviewer | Role | Weight | Score | Trend |
|----------|------|--------|-------|-------|
| Alex | Lead Reviewer | 25% | 75/100 | ↓3 |
| Sam | Security Lead | 25% | 85/100 | → |
| Jordan | Performance Lead | 15% | 60/100 | ↓8 |
| Taylor | Quality Lead | 15% | 55/100 | ↓5 |
| Riley | Reuse Lead | 20% | 45/100 | ↓12 |
```

对 high/critical finding，可附加影响说明：

> **Impact on Overall Score**: Fixing this duplication would increase Riley's score from 45 to 68, raising Overall Score from 66 to 71.

### 8.8 配置设计

```toml
[scoring]
enabled = true
display_individual_scores = true
display_weighted_score = true
display_score_impact = true

[scoring.penalties]
critical = 25
high = 15
medium = 8
low = 3
note = 0

[scoring.consensus]
multiplier_per_agreement = 0.2
max_multiplier = 2.0

[scoring.risk_levels]
critical_max = 29
high_max = 49
medium_max = 69
low_medium_max = 89
low_max = 100
```

### 8.9 评分校准与反馈闭环

收集数据：

```rust
pub struct ReviewScoreRecord {
    pub review_id: String,
    pub repo: String,
    pub mr_id: Option<String>,
    pub overall_score: u8,
    pub expert_scores: Vec<ExpertScoreRecord>,
    pub risk_level: RiskLevel,
    pub timestamp: DateTime<Utc>,
}

pub struct ExpertScoreRecord {
    pub expert_name: String,
    pub score: u8,
    pub weight: u8,
    pub findings_count: usize,
    pub helpful_findings: usize,
    pub false_positives: usize,
}
```

校准指标：

| 指标 | 说明 | 目标 |
|------|------|------|
| 命中率 | 被采纳并修复的 finding / 总 finding | > 60% |
| 误报率 | 被标记为 not helpful 的 finding / 总 finding | < 20% |
| Score-action 相关性 | 低 Overall Score 的 MR 是否真的问题更多 | 定期人工抽样 |
| 专家间一致性 | 同一问题被多名专家发现的比率 | 适中 |

每季度根据反馈调整 `weight` 和 prompt 判断标准。

### 8.10 边界情况

- **某专家无 finding**：score 为 100，报告中显示 "No findings from Jordan (Performance)"
- **某专家被禁用**：不计入权重，启用专家权重之和仍需为 100
- **Confidence < 6**：被 Lead 过滤或降级为 Note，不参与 score 计算

---

## 九、与现有文档的关系

本文档是 **团队层的产品设计规范**，其他技术文档需要与之对齐：

| 文档 | 需要更新的内容 |
|------|---------------|
| [`../.notes/review_engine_rs_roadmap.md`](../.notes/review_engine_rs_roadmap.md) | Virtual Team 设计、专家角色定义、输出标准、本地仓库支持、评分体系 |
| `code-audit-default.toml` | 专家 prompt 专业化、角色画像、权重配置 |
| `review_engine_large_pr_strategy.md` | 大 PR 下的团队协作流程 |
| `repo_aware_review_strategy.md` | 将 repo browser 定位为 "专业尽调工具"，支持本地路径 |
| [`../.notes/full_repo_review_strategy.md`](../.notes/full_repo_review_strategy.md) | 仓库级审核的团队分级、本地仓库审核、健康评分 |
| [`../.notes/pr_agent_learnings.md`](../.notes/pr_agent_learnings.md) | 增加 prompt 风格、输出标准的学习 |

---

## 十、下一步行动

1. 更新所有 expert prompt，加入专业输出标准
2. 在 `Finding` 结构体中增加 `evidence`、`impact`、`effort`、`references`
3. 实现 Lead Reviewer 的汇总和过滤逻辑
4. 设计报告模板，统一 finding 卡片格式，包含专家评分表
5. 实现 `expert_score` 和 `weighted_overall_score` 计算逻辑
6. 实现权重校验和 `[scoring]` 配置解析
7. 建立用户反馈收集机制
