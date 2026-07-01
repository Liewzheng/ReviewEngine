---
title: 专家聚合上下文增强方案
description: 让 aggregator 在整合各专家报告时获得完整的 MR 上下文（title、description、Lead 概述），提升整合报告质量
tags:
  - aggregator
  - architecture
  - prompt
related:
  - ../src/orchestrator.rs
  - ../src/team/orchestrator.rs
  - ../src/prompt/template.rs
  - ../src/lib.rs
  - ../.notes/review_engine_rs_roadmap.md
---

# 专家聚合上下文增强方案

> 目标：让 aggregator（LLM 聚合专家）在合并各专家报告时，拥有完整的 PR 上下文（标题、描述、Lead 概述），生成更有全局视野的整合报告。

---

## 一、问题现状

```
run_experts(experts, mr_info, diff, ...)
  → 内部计算 global_context（Pass 1 Lead 概述）
  → 注入到各 expert 的 prompt
  → 返回 Vec<ExpertReport>
  ✗ global_context 被丢弃，未返回给调用方

run_aggregator(aggregator, reports, llm_configs, ...)
  → 只拿到 reports（各专家的发现列表）
  → 没有 mr_info（标题/描述/分支信息）
  → 没有 global_context（Lead 的项目概况）
  → LLM 整合时缺乏上下文，输出片面
```

**核心缺口**：`run_experts` 内部计算了 `global_context` 但没返回。`run_aggregator` 只有专家报告，看不到 PR 全貌。

---

## 二、设计方案

### 2.1 `run_experts` 额外返回 `global_context`

```rust
// 当前
pub async fn run_experts(...) -> Result<Vec<ExpertReport>>

// 改为
pub async fn run_experts(...) -> Result<(Vec<ExpertReport>, Option<GlobalReviewContext>)>
```

所有现有调用者只需改一处解构：

```rust
// 旧
let reports = run_experts(...).await?;

// 新
let (reports, global_context) = run_experts(...).await?;
```

### 2.2 `run_aggregator` 接收 MR 上下文

```rust
pub async fn run_aggregator(
    aggregator: &ExpertDef,
    reports: &[ExpertReport],
    llm_configs: &[LLMConfig],
    mr_info: &MRInfo,                            // ← 新增
    global_context: Option<&GlobalReviewContext>,  // ← 新增
    progress_map: Option<ProgressMap>,
    review_id: &str,
) -> Result<AggregatedReport>
```

### 2.3 聚合 prompt 模板增强

当前模板只给了 `reports`。新增 `mr_title`、`mr_description`、`source_branch`、`target_branch`、`global_context` 等字段。

```jinja
## Pull Request Context

**Title**: {{ mr_title }}
**Description**: {{ mr_description }}
**Branches**: {{ source_branch }} → {{ target_branch }}
**Author**: {{ pr_author }}

{% if global_context %}
## Lead Overview

**Summary**: {{ global_context.summary }}
**Risk Areas**: {{ global_context.risk_areas | join(", ") }}
**Focus Files**: {{ global_context.focus_files | join(", ") }}
**Guidance**: {{ global_context.guidance }}
{% endif %}
```

这样 LLM 聚合时就知道 "这个 PR 是改什么的、风险在哪、Lead 重点关注什么"。

### 2.4 调用链更新

```
lib.rs::run_review()
  → run_experts(...) → (reports, gctx)
  → if aggregated:
      run_aggregator(aggregator, &reports, &llm_configs, &mr_info, gctx.as_ref(), ...)

server/gitlab.rs::run_review_for_mr()
  → run_experts(...) → (reports, gctx)
  → run_aggregator(aggregator, &reports, &llm_configs, &mr_info, gctx.as_ref(), ...)

server/github.rs::run_review_for_pr()
  → run_experts(...) → (reports, gctx)
  → run_aggregator(aggregator, &reports, &llm_configs, &mr_info, gctx.as_ref(), ...)
```

CLI 本地路径（`run_local`、`run_local_repo`）不需要 global_context，只需更新解构：

```rust
let (reports, _) = run_experts(...).await?;
```

---

## 三、文件变更清单

| 文件 | 变更 | 行数 |
|------|------|------|
| `src/team/orchestrator.rs` | `run_experts` 返回 `(Vec<ExpertReport>, Option<GlobalReviewContext>)` | +3 |
| `src/orchestrator.rs` | re-export 自动同步 | — |
| `src/prompt/template.rs` | `build_aggregator_prompt` 新增 `mr_info`、`global_context` 参数 + 模板变量 | +15 |
| `src/lib.rs` | 解构 `(reports, gctx)` 并传给 `run_aggregator` | +3 |
| `src/server/gitlab.rs` | 同上 | +3 |
| `src/server/github.rs` | 同上 | +3 |
| `src/main.rs` | 解构 `(reports, _)`（CLI local/local_repo 路径共 2 处） | +4 |
| **合计** | | **~31 行** |

---

## 四、进度体现

aggregator 已纳入 `--progress` 进度系统（`src/progress/mod.rs`）：

| PR 类型 | stage name | 标签 | 权重 |
|---------|-----------|------|------|
| 小 PR | `aggregate` | `Aggregating reports` | 8% |
| 大 PR | `aggregate` | `Aggregating reports` | 8% |

`run_aggregator` 结束时已调用 `progress.complete_stage("aggregate")`，进度条会从 `expert_review（85%/70%）` 过渡到 `aggregate（8%）` 再到完成。

**改进点**：聚合开始时加一行 `set_stage`，让进度条显示"运行中"状态而非从 `expert_review` 直接跳到完成：

```rust
// run_aggregator 开头：标记 aggregate 阶段开始
if let Some(ref map) = progress_map {
    if let Ok(mut p) = map.write() {
        if let Some(progress) = p.get_mut(review_id) {
            progress.set_stage("aggregate", 0.5, "Aggregating expert reports...".to_string());
        }
    }
}
```

这样 `--progress` 在聚合阶段会显示 `🔄 Aggregating expert reports`，体验更连续。

---

## 五、不做的事

| 事项 | 理由 |
|------|------|
| 完整 diff 传给 aggregator | 大 PR diff 可能几十 KB，远超 token 预算。mr_info + global_context 提供了足够的语义上下文 |
| TeamRenderer + LeadConsolidator 确定性整合 | 作为 `aggregated = false` 时的降级方案，下一阶段实现 |
| `build_aggregator_prompt` 重写 | 只加字段，不改模板结构 |
