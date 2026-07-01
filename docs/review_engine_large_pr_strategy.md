---
title: review_engine 大 PR 审核方案
description: 压缩、分块、专家路由、并行审核与结果聚合的完整方案
tags:
  - review-engine
  - large-pr
  - chunking
  - performance
related:
  - ../.notes/review_engine_rs_roadmap.md
  - professional_team_design.md
---
# review_engine 大 PR 审核方案

> 目标：让虚拟研发团队（Virtual Engineering Team）能够高效、完整、低成本地审核超过模型上下文窗口的大型 PR。
> 核心思想：**压缩 → 概览 → 分块 → 路由 → 并行 → 聚合**。

---

## 一、问题定义

### 1.1 什么是“大 PR”

触发大 PR 处理流程的条件（满足任一即可）：

```rust
pub fn is_large_pr(diff: &DiffSummary, config: &DiffConfig) -> bool {
    diff.total_tokens > config.max_input_tokens - config.output_buffer_tokens
        || diff.files.len() > config.large_pr_file_threshold      // e.g. 50
        || diff.total_lines_changed > config.large_pr_line_threshold  // e.g. 5000
}
```

### 1.2 大 PR 审核的难点

| 难点 | 影响 |
|------|------|
| 超出模型上下文窗口 | 无法一次性送入 LLM |
| 信息密度低 | 大量删除/格式化/生成的噪音 |
| 跨文件关联 | 分块后容易丢失模块级上下文 |
| 成本爆炸 | 每个专家都看全量 diff，token 消耗巨大 |
| 结果碎片化 | 多个 chunk 产生重复或冲突 finding，难以阅读 |

### 1.3 设计原则

1. **不该看的就不看**：过滤、压缩、路由，减少无效 token。
2. **先全局再局部**：先让架构/lead 专家把握整体，再让领域专家深入 chunk。
3. **专家各有所看**：安全专家只看安全相关文件，性能专家只看热点代码。
4. **结果可合并**：跨 chunk 的 finding 必须去重、排序、冲突标注。

---

## 二、整体流程

```
原始大 PR diff
    │
    ▼
┌─────────────────────────────────────────┐
│  Step 1: 预过滤与压缩                    │
│  - 去二进制/生成文件                     │
│  - 删除纯删 hunk，合并删除列表            │
│  - 减少上下文行                          │
│  - 按语言/token 排序                     │
└─────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────┐
│  Step 2: 是否能装入单 prompt？           │
│  是 → 走普通 review 流程                 │
│  否 → 进入大 PR 流程                     │
└─────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────┐
│  Step 3: 概览审核（Pass 1）              │
│  Lead/Architecture 专家看全局摘要        │
│  输出：风险地图 + 重点关注区域            │
└─────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────┐
│  Step 4: 分块（Chunking）                │
│  水平分块 / 垂直分块 / 语义分块           │
└─────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────┐
│  Step 5: 专家路由（Routing）             │
│  每个 chunk 分配给最相关的专家            │
└─────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────┐
│  Step 6: 并行深度审核（Pass 2）          │
│  多名专家并行审多个 chunk                 │
└─────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────┐
│  Step 7: 聚合与呈现                      │
│  去重、排序、冲突标注、生成团队报告        │
└─────────────────────────────────────────┘
```

---

## 三、三级压缩策略

### 3.1 第一级：文件级过滤

**目标**：把完全不需要 LLM 看的文件剔除。

**规则**：

- 移除 binary 文件（图片、字体、编译产物）
- 移除 generated 文件（`Cargo.lock`、`yarn.lock`、`package-lock.json`、protobuf 生成代码）
- 按 `.code-audit-config.toml` 的 `ignore_files`/`ignore_paths` 过滤（当前仍兼容 `.pr-agent.toml`，将弃用）
- 标记 `is_generated = true` 的文件直接跳过内容，只保留文件名

**配置**：

```toml
[diff]
ignore_files = ["*.lock", "*.min.js", "*.min.css", "dist/**"]
```

---

### 3.2 第二级：Hunk 级压缩

**目标**：在保留关键信息的前提下减少 diff 体积。

**规则**：

1. **删除纯删除 hunk**：只保留文件名，放入 `Deleted files:` 列表
2. **合并相邻小 hunk**：同一文件内距离很近的 hunk 合并
3. **减少上下文行**：
   - 小 PR：`patch_extra_lines = 3`
   - 大 PR：`patch_extra_lines = 1` 或 `0`
4. **截断超长行**：单行超过 500 字符时截断并标注 `...truncated`

**效果**：通常能减少 30%–60% 的 token。

---

### 3.3 第三级：文件优先级排序

**目标**：当 token 不够时，确保最重要的文件进入 prompt。

**排序规则**：

```rust
pub fn file_priority(file: &DiffFile, repo_languages: &[LanguageStat]) -> PriorityScore {
    let mut score = 0;

    // 1. 主语言优先
    if is_main_language(file, repo_languages) {
        score += 1000;
    }

    // 2. 按 token 数排序（变更多的文件更重要）
    score += file.tokens.min(5000);

    // 3. 新增/修改文件优先于删除文件
    match file.edit_type {
        ADDED | MODIFIED => score += 200,
        RENAMED => score += 100,
        DELETED => score += 0,
    }

    // 4. 敏感模式加权
    if matches_security_patterns(file.path) {
        score += 500;
    }
    if matches_api_patterns(file.path) {
        score += 300;
    }

    score
}
```

**结果**：文件按优先级从高到低装入 prompt，装不下的文件进入 `Additional modified files` 列表。

---

## 四、分块（Chunking）算法

### 4.1 水平分块：按文件分组（默认策略）

适合：**文件多但单文件不大**的 PR。

**算法**：

```rust
pub fn chunk_by_files(
    files: &[DiffFile],
    max_tokens_per_chunk: usize,
) -> Vec<Vec<DiffFile>> {
    let mut chunks: Vec<Vec<DiffFile>> = vec![];
    let mut current = vec![];
    let mut current_tokens = 0;

    for file in files {
        let file_tokens = file.tokens;
        if current_tokens + file_tokens > max_tokens_per_chunk && !current.is_empty() {
            chunks.push(current);
            current = vec![];
            current_tokens = 0;
        }
        current_tokens += file_tokens;
        current.push(file.clone());
    }

    if !current.is_empty() {
        chunks.push(current);
    }

    chunks
}
```

**优点**：
- 实现简单
- 天然适合并行
- 每个 chunk 独立，失败不影响其他 chunk

**缺点**：
- 跨文件关联关系可能丢失
- 如果某个文件巨大，会撑爆单个 chunk

---

### 4.2 垂直分块：按 Hunk 拆分

适合：**单个文件变更极大**的 PR。

**算法**：

```rust
pub fn chunk_by_hunks(
    file: &DiffFile,
    max_tokens_per_chunk: usize,
) -> Vec<String> {
    let mut chunks: Vec<String> = vec![];
    let mut current = String::new();
    let mut current_tokens = 0;

    for hunk in &file.hunks {
        let hunk_text = render_hunk(hunk);
        let hunk_tokens = count_tokens(&hunk_text);

        if current_tokens + hunk_tokens > max_tokens_per_chunk && !current.is_empty() {
            chunks.push(current);
            current = format!("## File: '{}\n", file.path);  // 保留文件头
            current_tokens = count_tokens(&current);
        }

        current.push_str(&hunk_text);
        current_tokens += hunk_tokens;
    }

    if !current.is_empty() {
        chunks.push(current);
    }

    chunks
}
```

**优点**：
- 能处理单文件超大 diff
- 保留文件头作为上下文

**缺点**：
- 同一文件不同 chunk 之间上下文减少
- 需要额外记录每个 chunk 对应的行号范围

---

### 4.3 语义分块：按模块/目录

适合：**模块边界清晰**的大型项目。

**算法**：

```rust
pub fn chunk_by_module(files: &[DiffFile], max_tokens: usize) -> Vec<ModuleChunk> {
    // 按目录前缀分组，如 src/auth/、src/db/、src/api/
    // 每组尽量保持完整，超 token 再拆
}
```

**优点**：
- 保留模块内上下文
- 便于路由给领域专家

**缺点**：
- 模块大小不均衡时难平衡 token
- 需要项目结构知识

---

### 4.4 推荐策略：自适应混合

```rust
pub fn adaptive_chunk(files: &[DiffFile], config: &ChunkConfig) -> Vec<Chunk> {
    // 1. 先尝试水平分块
    let mut chunks = vec![];

    for group in chunk_by_files(files, config.max_tokens_per_chunk) {
        for file in group {
            if file.tokens > config.single_file_chunk_threshold {
                // 单文件太大，垂直分块
                chunks.extend(chunk_by_hunks(&file, config.max_tokens_per_chunk));
            } else {
                // 普通文件，保留在水平 chunk 中
                // ...
            }
        }
    }

    chunks
}
```

**实现顺序**：
1. Phase 1：水平分块
2. Phase 2：垂直分块兜底
3. Phase 3：语义分块优化

---

## 五、两阶段审核（Two-Pass Review）

### 5.1 Pass 1：概览审核

**执行者**：Lead / Architecture 专家（固定参与）。

**输入**：
- 压缩后的全局 diff（尽量完整但精简）
- 文件列表与变更统计
- 项目主语言、commit messages

**输出格式**（YAML）：

```yaml
overview:
  summary: "本次 PR 主要重构了认证模块，并新增了 OAuth2 支持"
  risk_level: "high"
  key_areas:
    - file: "src/auth/oauth2.rs"
      reason: "新增外部 HTTP 调用，需关注错误处理和安全"
    - file: "src/db/migrations/2024_xxx.sql"
      reason: "数据库 schema 变更，需关注回滚和兼容性"
  recommended_expert_focus:
    security: ["src/auth/**", "src/db/**"]
    performance: ["src/auth/oauth2.rs"]
    testing: ["src/auth/**"]
```

**作用**：
- 让 lead 专家把握全局
- 为 Pass 2 的专家路由提供依据
- 识别跨文件风险和架构问题

---

### 5.2 Pass 2：深度分块审核

**执行者**：根据路由分配的专家团队。

**输入**：
- 来自 Pass 1 的风险地图
- 分配给该专家的 chunk
- 专家自身的 perspective 和 focus
- RepoBrowser 上下文（完整文件、相关测试、调用关系）

**输出**：每个 chunk 的 finding 列表，符合专业 finding 标准（evidence、impact、recommendation、effort）。

### 5.3 Lead 协调与质量把控

大 PR 下专家数量多、chunk 多，更容易出现：
- 同一问题被多个专家在不同 chunk 中发现
- 专家之间对同一设计决策意见冲突
- 低质量 finding 混入

Lead Reviewer 在大 PR 中的特殊职责：
- 审核每个 expert 的 finding，删除 confidence < 6 或缺少 evidence 的项
- 识别跨 chunk 的重复发现并合并
- 对冲突意见组织 Reviewer Discussion，给出 resolution
- 生成整体 Risk Level 和 TL;DR
- 确保 critical/high finding 都有明确 action

---

## 六、专家路由（Expert Routing）

### 6.1 核心思想

不是每个专家都看所有 chunk。不同专家根据文件类型、变更内容、风险地图，只看到自己最该看的内容。

### 6.2 路由规则示例

```toml
[team.security]
trigger = "conditional"
file_patterns = ["**/*auth*", "**/*login*", "**/*crypto*", "**/*permission*", "**/db/**"]
content_patterns = ["password", "token", "secret", "sql", "exec", "eval"]

[team.performance]
trigger = "conditional"
file_patterns = ["**/*.rs", "**/*.cpp", "**/*.go"]
content_patterns = ["for ", "while ", "loop", "mutex", "lock", "await", "query"]

[team.api]
trigger = "conditional"
file_patterns = ["**/api/**", "**/openapi*", "**/*.proto", "**/routes/**"]
```

### 6.3 路由决策流程

```rust
pub fn route_chunks(
    chunks: &[Chunk],
    experts: &[ExpertDef],
    risk_map: &RiskMap,
) -> Vec<ExpertChunkAssignment> {
    let mut assignments = vec![];

    for expert in experts {
        if expert.commands.contains("review") {
            // 参与 review 命令的核心专家：看所有 chunk（或最重要的前 N 个）
            for chunk in chunks.iter().take(expert.max_chunks.unwrap_or(3)) {
                assignments.push(ExpertChunkAssignment {
                    expert: expert.clone(),
                    chunk: chunk.clone(),
                    context: Some(risk_map.summary.clone()),
                });
            }
        } else {
            // Domain Expert：只匹配相关 chunk
            for chunk in chunks {
                if expert.matches(chunk) {
                    assignments.push(ExpertChunkAssignment {
                        expert: expert.clone(),
                        chunk: chunk.clone(),
                        context: None,
                    });
                }
            }
        }
    }

    assignments
}
```

### 6.4 成本控制

```toml
[team]
max_team_size = 6
max_chunks_per_expert = 3
max_total_llm_calls = 20
```

---

## 七、并行调度

### 7.1 并行层次

```
不同 expert 之间：并行
同一 expert 的不同 chunk：并行
单个 chunk 的 LLM 调用：async 单线程
```

### 7.2 实现示例

```rust
pub async fn run_deep_review(assignments: &[ExpertChunkAssignment]) -> Vec<ExpertReport> {
    let semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT_LLM_CALLS));

    let tasks: Vec<_> = assignments
        .iter()
        .map(|assignment| {
            let permit = semaphore.clone().acquire_owned().await.unwrap();
            tokio::spawn(async move {
                let _permit = permit;
                review_chunk(&assignment).await
            })
        })
        .collect();

    let results: Vec<Result<ExpertReport, _>> = join_all(tasks).await;
    results.into_iter().filter_map(|r| r.ok()).collect()
}
```

### 7.3 速率限制

- 对单一 LLM provider 设置 RPS 上限
- 使用 token bucket 或 semaphore 控制并发
- 遇到 429 时指数退避

---

## 八、结果聚合

### 8.1 Finding 标准化

```rust
pub struct Finding {
    pub file: String,
    pub line: Option<u32>,
    pub severity: String,       // critical | high | medium | low
    pub title: String,
    pub detail: String,
    pub confidence: u8,         // 0-10
    pub expert_name: String,
    pub expert_role: String,
    pub chunk_id: Option<String>,
}
```

### 8.2 去重键

```rust
pub fn finding_key(f: &Finding) -> String {
    let normalized_title = f.title.to_lowercase()
        .replace("should be", "")
        .replace("consider", "")
        .replace(|c: char| !c.is_alphanumeric(), "");

    format!(
        "{}:{}:{}",
        f.file,
        f.line.unwrap_or(0),
        normalized_title
    )
}
```

### 8.3 聚合规则

| 情况 | 处理方式 |
|------|---------|
| 完全重复（同 key） | 合并为一条，标注多名 reviewer 共识 |
| 邻近重复（同文件、行号差 ≤ 3、标题相似） | 合并为一条，保留最详细描述 |
| 冲突意见（同位置不同建议） | 都保留，单独列出 “💬 Conflicting views” |
| 低置信度 | 放入折叠区 |
| 评分计算 | 跨 chunk 的 finding 仍按专家 weight 计算 Overall Score |

### 8.4 排序规则

```rust
pub fn finding_rank(f: &Finding) -> u32 {
    let severity_score = match f.severity.as_str() {
        "critical" => 1000,
        "high" => 800,
        "medium" => 500,
        "low" => 200,
        _ => 0,
    };

    let confidence_score = (f.confidence as u32) * 10;
    let consensus_bonus = f.agreeing_experts.len() as u32 * 50;

    severity_score + confidence_score + consensus_bonus
}
```

---

## 九、配置设计

```toml
[diff]
max_input_tokens = 120000
output_buffer_tokens = 2000
large_pr_file_threshold = 50
large_pr_line_threshold = 5000

# 压缩策略
compression_level = "aggressive"   # none | normal | aggressive
patch_extra_lines = 1              # 大 PR 减少上下文
max_line_length = 500              # 截断超长行

# 分块策略
chunking_strategy = "adaptive"     # file_based | hunk_based | semantic | adaptive
max_tokens_per_chunk = 30000
max_files_per_chunk = 20
single_file_chunk_threshold = 25000  # 单文件超过此值走垂直分块

[team]
# 概览审核
overview_expert = "lead"

# 路由与并发
max_team_size = 6
max_chunks_per_expert = 3
max_concurrent_llm_calls = 6
llm_rate_limit_rps = 10

# 团队模式
team_mode = "dynamic"              # core_only | dynamic | thorough
```

---

## 十、性能目标

| 指标 | 小 PR | 大 PR（>100 文件） |
|------|------|------------------|
| 首条 finding | ≤ 5s | ≤ 10s |
| 完整团队报告 | ≤ 30s | ≤ 90s |
| Token 节省（vs 无压缩全量） | 20% | ≥ 50% |
| Cost 降低（vs 每个专家看全量） | 10% | ≥ 40% |
| Finding 重复率 | - | ≥ 50% 自动合并 |

---

## 十一、建议代码模块

```
src/diff/
  ├── compression.rs      # 三级压缩
  ├── chunker.rs          # 水平/垂直/自适应分块
  └── prioritizer.rs      # 文件优先级排序

src/review/
  ├── overview.rs         # Pass 1 概览审核
  └── deep_review.rs      # Pass 2 深度审核

src/team/
  ├── router.rs           # chunk → expert 路由
  ├── scheduler.rs        # 并发调度、rate limit
  └── aggregator.rs       # 结果聚合（增强）
```

---

## 十二、与主路线图的衔接

本方案应作为 **Phase 1 核心评审引擎加固** 的深化内容：

1. **3.1 精确 Token Budget 与 Diff 压缩** 完成后，立即接入本方案。
2. **3.5 虚拟团队编排器** 需要依赖 `chunker` 和 `router`。
3. **3.4 团队聚合器** 需要处理来自多个 chunk 的 finding。
4. **Phase 2 GitLab Agent 化** 时，大 PR 的处理对用户完全透明——用户只感到“虽然 PR 很大，但 review 依然很快”。

---

## 十三、验收清单

- [ ] 50+ 文件、10k+ 行变更的 MR 能在 90 秒内完成 review
- [ ] 压缩后 token 数比原始 diff 减少 ≥ 50%
- [ ] 单文件超大 diff（>25k tokens）能正确垂直分块
- [ ] 不同专家只看到相关 chunk，总 LLM cost 比全量模式降低 ≥ 40%
- [ ] 跨 chunk 的重复 finding 自动合并率 ≥ 50%
- [ ] 大 PR 报告顶部显示概览风险地图、Overall Assessment 和 Overall Score (0-100)
- [ ] Lead Reviewer 过滤掉 confidence < 6 或缺少 evidence 的 finding
- [ ] 每条 critical/high finding 都有 impact、recommendation 和 effort
- [ ] 专家冲突由 Lead 给出专业 resolution
- [ ] 配置可切换 `chunking_strategy` 和 `compression_level`
