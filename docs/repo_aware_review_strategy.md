---
title: review_engine 仓库感知型 MR 审核方案
description: 让专家像真人工程师一样查阅仓库上下文、验证假设，并支持本地仓库模式
tags:
  - review-engine
  - repo-browser
  - context
  - local-repo
related:
  - ../.notes/full_repo_review_strategy.md
  - ../.notes/review_engine_rs_roadmap.md
  - professional_team_design.md
---
# review_engine 仓库感知型 MR 审核方案

> 目标：让 MR 审核专家不再只盯着 diff，而是能像真人工程师一样在仓库中查阅上下文、搜索模式、验证假设。
> 核心命题：**diff 只是入口，仓库才是真相来源。专业的 reviewer 不会只看 diff。**

---

## 一、问题：只看 diff 的审核是盲人摸象

当前 review_engine_rs 的审核流程：

```
MR diff → expert → finding
```

这相当于把 diff 打印出来给 reviewer 看，但**不允许他打开 IDE、不允许他搜索仓库、不允许他看相关文件**。

### 1.1 真实工程师会看什么

一个 senior engineer review PR 时，绝不会只看 diff。他会：

- 打开被修改的文件，看完整上下文
- 查看这个函数被谁调用、调用了谁
- 搜索仓库里是否有类似实现
- 检查测试文件是怎么写的
- 看看相关模块的约定和模式
- 确认这个改动是否破坏了某个隐式契约

### 1.2 只看 diff 会漏掉什么

| 场景 | diff 能看到 | 仓库上下文能看到 |
|------|------------|----------------|
| 新增函数签名 | 函数定义 | 调用者是否已更新、测试是否覆盖 |
| 修改工具函数 | 改动本身 | 其他 10 个文件是否也依赖这个函数 |
| 新增错误处理 | 当前文件 | 项目里其他模块如何处理同类错误 |
| 重命名变量 | 当前文件 | 文档、配置文件、测试里是否还有旧名 |
| 新增配置项 | 当前文件 | 是否有对应的配置校验、默认值、文档 |
| 修改数据库查询 | 当前文件 | 是否有索引、是否已有 N+1 模式 |

### 1.3 为什么这是专业化的一部分

专业 reviewer 的判断不是凭空产生的，而是基于：
- **证据**：代码片段、调用链、测试覆盖
- **上下文**：项目约定、历史模式、相关模块
- **标准**：OWASP、SOLID、DRY、团队规范
- **可追溯**：每条 finding 都能说明依据

仓库感知型审核把「查资料、验证假设」的过程显式化，让 AI 的 finding 更像真人工程师经过尽职调查后的结论。

---

## 二、愿景：给每个专家配一个「仓库浏览器」

让每个 expert 在执行 review 时，可以按需查询仓库信息：

```rust
pub trait RepoBrowser {
    async fn get_file(&self, path: &str, ref: &str) -> Result<String>;
    async fn search_code(&self, query: &str) -> Result<Vec<SearchResult>>;
    async fn find_callers(&self, file: &str, symbol: &str) -> Result<Vec<Location>>;
    async fn find_callees(&self, file: &str, symbol: &str) -> Result<Vec<Location>>;
    async fn get_related_tests(&self, file: &str) -> Result<Vec<String>>;
    async fn get_file_history(&self, path: &str, limit: usize) -> Result<Vec<Commit>>;
    async fn find_similar_functions(&self, func: &FunctionSignature) -> Result<Vec<FunctionSignature>>;
}
```

专家在 prompt 里被明确告知：

> "你可以使用仓库浏览器查询以下信息来验证你的判断：完整文件内容、调用关系、相似实现、测试覆盖、历史变更。"

### 2.1 本地仓库是首选数据源

仓库浏览器必须同时支持两种后端：

| 后端 | 场景 | 优势 |
|------|------|------|
| **本地 git repo** | 本地开发、CI、预提交、全仓库审核 | 无网络延迟、可读取 working tree、可访问完整历史 |
| **Git provider API** | GitLab/GitHub MR 审核、无需 clone | 远程触发、与 MR 状态集成 |

本地模式是远程模式的基础能力：
- 本地路径下直接 `git diff` 即可获取变更
- 文件读取走文件系统，响应速度 < 100ms
- 不依赖 API token，适合本地快速验证
- 同一套专家流程和报告格式，无缝切换远程/本地

```rust
pub enum RepoBrowserBackend {
    LocalGit { path: PathBuf },
    GitLabApi { client: GitLabClient },
    GitHubApi { client: GitHubClient },
}
```

---

## 三、专家需要哪些上下文工具

### 3.1 文件级查询

| 工具 | 作用 | 使用场景 |
|------|------|---------|
| `get_file(path)` | 获取文件完整内容 | diff 只展示片段，需要看完整函数 |
| `get_file_range(path, start, end)` | 获取指定行范围 | 看函数上下文而不加载整个大文件 |
| `get_file_at_ref(path, ref)` | 获取目标分支版本 | 对比修改前后的完整文件 |

### 3.2 符号级查询

| 工具 | 作用 | 使用场景 |
|------|------|---------|
| `find_callers(symbol)` | 谁调用了这个函数 | 判断改动影响范围 |
| `find_callees(symbol)` | 这个函数调用了谁 | 判断依赖是否合法 |
| `find_definitions(symbol)` | 符号定义位置 | 跳转定义 |

### 3.3 模式搜索

| 工具 | 作用 | 使用场景 |
|------|------|---------|
| `search_code(pattern)` | 文本/正则搜索 | 找相似实现、找遗漏 |
| `find_similar_functions(func)` | 找语义相似函数 | 识别可复用机会 |
| `find_duplicate_blocks()` | 找重复代码块 | 复用专家的核心工具 |

### 3.4 关系查询

| 工具 | 作用 | 使用场景 |
|------|------|---------|
| `get_related_tests(file)` | 找对应测试文件 | 测试专家检查覆盖 |
| `get_imports(file)` | 获取文件依赖 | 架构专家检查耦合 |
| `get_dependents(module)` | 获取依赖该模块的模块 | 影响范围分析 |

### 3.5 历史查询

| 工具 | 作用 | 使用场景 |
|------|------|---------|
| `get_file_history(path)` | 文件最近变更 | 判断代码是否频繁改动（热点） |
| `get_blame(path, line)` | 某行最后修改者 | 需要进一步沟通时 |

---

## 四、两种实现路径

### 路径 A：预取上下文（Pre-fetch）

在 expert 运行前，先自动收集好相关上下文：

```rust
pub fn gather_context(diff: &Diff, browser: &RepoBrowser) -> ReviewContext {
    ReviewContext {
        changed_files_full: fetch_full_files(diff, browser),
        related_files: find_related_files(diff, browser),
        similar_patterns: find_similar_patterns(diff, browser),
        test_files: find_related_tests(diff, browser),
    }
}
```

**优点**：
- 一次 LLM 调用，context 完整
- 简单可控

**缺点**：
- 上下文可能太多，超 token
- 可能取到不相关的信息

---

### 路径 B：工具调用（Tool Use / Function Calling）

让 LLM 自己决定什么时候查询什么：

```rust
pub async fn review_with_tools(expert: &Expert, diff: &Diff, browser: &RepoBrowser) -> ExpertReport {
    let mut messages = vec![system_prompt(expert), user_prompt(diff)];

    loop {
        let response = llm.chat(messages.clone()).await;

        if let Some(tool_call) = response.tool_call {
            let result = execute_tool(tool_call, browser).await;
            messages.push(tool_result_message(result));
        } else {
            return parse_expert_report(response.content);
        }
    }
}
```

**优点**：
- 更精确，只查需要的信息
- 更像真人 reviewer
- token 更高效

**缺点**：
- 实现复杂
- 需要 LLM 支持 function calling
- 延迟可能更高（多轮交互）

---

### 推荐策略

**Phase 1：预取上下文**

先实现路径 A，把常用上下文自动塞进 prompt。这是性价比最高的方案。

**Phase 2：混合模式**

对复杂情况启用工具调用。例如：
- 默认预取上下文
- 当 expert 输出 finding 但 confidence 低时，自动触发一次 `search_code` 验证

**Phase 3：纯工具调用**

等 LLM function calling 成熟后，全面转向路径 B。

---

## 五、每个专家的上下文增强

### 5.1 Security 专家

预取上下文：
- 被修改文件的完整内容
- 所有 import/use 语句
- 仓库中其他安全相关文件（auth、crypto、db）

查询能力：
- `search_code("password|secret|token|api_key")` — 找硬编码
- `find_callers("authenticate")` — 看认证函数调用点
- `get_related_tests(file)` — 检查安全测试覆盖

---

### 5.2 Performance 专家

预取上下文：
- 被修改函数附近 100 行
- 该函数的调用链

查询能力：
- `find_callers("slow_function")` — 判断热点
- `search_code("for.*in.*range|while True")` — 找循环模式
- `get_file_history(path)` — 看是否频繁优化/修复

---

### 5.3 Architecture 专家

预取上下文：
- 模块依赖图
- 被修改文件所属模块的其他文件

查询能力：
- `get_imports(file)` — 看耦合
- `get_dependents(module)` — 看影响范围
- `find_similar_functions(func)` — 看是否有更好的抽象

---

### 5.4 Reuse 专家（Riley）

预取上下文：
- 被修改文件的完整内容
- 仓库中相同语言的所有文件列表

查询能力：
- `find_duplicate_blocks()` — 核心能力
- `search_code("similar_function_signature")` — 找可合并实现
- `get_related_tests(file)` — 看测试是否重复

---

### 5.5 Testing 专家

预取上下文：
- 被修改文件
- 对应测试文件

查询能力：
- `get_related_tests(file)` — 找测试
- `search_code("test_xxx")` — 检查命名约定
- `get_file_history(test_file)` — 看测试维护情况

---

## 六、本地仓库模式详解

### 6.1 为什么本地模式重要

专业 reviewer 在本地 IDE 里 review 时，拥有完整仓库的所有上下文。AI reviewer 也应该能这样工作。本地模式让 review_engine 可以：

- 在开发者提交前预审本地改动
- 在 CI 中不依赖第三方平台做代码审核
- 对私有/离线仓库做审核（无需上传代码到 SaaS）
- 作为全仓库健康检查的基础设施

| 场景 | 远程 MR 审核 | 本地仓库审核 |
|------|-------------|-------------|
| 开发者提交前预审 | 未创建 MR 时无法使用 | `review-engine review --local-path .` |
| 私有/离线仓库 | 需要上传代码 | 本地执行，代码不出境 |
| CI 预提交检查 | 依赖 webhook | 作为 CI step 直接调用 |
| 全仓库健康检查 | 需要平台 API | 直接扫描本地文件系统 |
| 快速调试 prompt | 需要真实 MR | 任意 commit range |

### 6.2 输入抽象

```rust
pub enum ReviewInput {
    GitLabMR { url: String },
    GitHubPR { url: String },
    LocalRepo {
        path: PathBuf,
        base_ref: Option<String>,
        head_ref: Option<String>,
        diff_source: LocalDiffSource,
    },
}

pub enum LocalDiffSource {
    WorkingTreeVsRef { base: String },
    Staged,
    Commits { since: String, until: String },
}
```

### 6.3 CLI 设计

```bash
# working tree vs main
review-engine review --local-path ./my-repo --base main

# staged changes
review-engine review --local-path ./my-repo --staged

# last 3 commits
review-engine review --local-path ./my-repo --since HEAD~3

# full repo review
review-engine repo-review --local-path ./my-repo --branch main

# CI gate
review-engine review --local-path . --base main --fail-on-risk-level high
```

### 6.4 本地 RepoBrowser 实现

```rust
pub struct LocalGitBrowser {
    pub repo_path: PathBuf,
}

impl RepoBrowser for LocalGitBrowser {
    async fn get_file(&self, path: &str, git_ref: &str) -> Result<String> {
        // git show <ref>:<path>
    }

    async fn search_code(&self, query: &str) -> Result<Vec<SearchResult>> {
        // rg / grep 本地搜索
    }

    async fn get_related_tests(&self, file: &str) -> Result<Vec<String>> {
        // 同名 *_test.rs 或 tests/ 下匹配文件
    }

    async fn get_file_history(&self, path: &str, limit: usize) -> Result<Vec<Commit>> {
        // git log -n <limit> -- <path>
    }
}
```

本地模式性能优势：

| 操作 | 远程 API | 本地文件系统 |
|------|---------|-------------|
| 读取单文件 | 100-500ms | < 1ms |
| 搜索代码 | 1-3s | 100-500ms |
| 获取文件历史 | 500ms-2s | 10-100ms |

### 6.5 本地报告差异

| 项目 | 远程 MR 报告 | 本地仓库报告 |
|------|-------------|-------------|
| 触发 | MR URL / Webhook | `--local-path` |
| 文件路径 | 仓库内相对路径 | 本地相对路径（可点击跳转） |
| 上下文 | target branch | `--base` 指定的 ref |
| 回写 | MR discussion | stdout / 文件 / CI 状态 |
| 显示 | MR 标题/作者 | 当前 branch / commit range |
| Overall Score | 有 | 有 |
| 专家评分表 | 有 | 有 |

### 6.6 典型使用场景

- **开发者本地预审**：`review-engine review --local-path . --base main`
- **pre-commit hook**：`--staged --fail-on-risk-level high`
- **CI 门禁**：`--base origin/main --fail-on-risk-level high`
- **全仓库健康检查**：`review-engine repo-review --local-path . --branch main`
- **离线/私有环境**：不依赖网络，代码不上传

### 6.7 与远程 MR 审核的共享能力

无论是本地还是远程，专家拿到的上下文格式一致：
- `ReviewInput` 统一抽象
- `RepoBrowser` 统一接口
- Expert prompt 不区分数据来源
- 报告模板相同，只是头部元数据不同

---

## 七、Prompt 设计

### 7.1 预取模式 Prompt

```markdown
You are {{ expert.name }}, a {{ expert.role }} reviewing this PR.

You have access to the following repository context:

## Changed files (full content)
{{ changed_files_full }}

## Related files
{{ related_files }}

## Similar patterns found in repo
{{ similar_patterns }}

## Related tests
{{ related_tests }}

## Instructions
- Focus only on issues introduced or missed by this PR.
- Use the repo context to verify your assumptions.
- If you see a function that looks like it should exist elsewhere, use the similar patterns to check.
- If you see a security-sensitive change, check related auth/db files for consistency.

Output findings in YAML format...
```

### 7.2 工具调用模式 Prompt

```markdown
You are {{ expert.name }}, a {{ expert.role }} reviewing this PR.

You can use the following tools to investigate the repository:
- `get_file(path)` — read full file content
- `search_code(pattern)` — search for patterns
- `find_callers(symbol)` — find who calls a function
- `get_related_tests(file)` — find test files

When you need more context to make a confident judgment, explicitly call a tool.
Do not guess. Verify your assumptions using the repository browser.
```

---

## 八、技术架构

```
MR diff / Local diff
  │
  ▼
┌─────────────────────────────┐
│  Context Gatherer           │
│  - 识别 diff 中涉及文件       │
│  - 决定需要预取哪些上下文     │
└─────────────────────────────┘
  │
  ▼
┌─────────────────────────────┐
│  Repo Browser               │
│  - 本地 git repo（首选）      │
│  - Git provider API         │
│  - 本地索引（AST/向量）      │
└─────────────────────────────┘
  │
  ▼
┌─────────────────────────────┐
│  Context Assembler          │
│  - 合并文件内容              │
│  - 控制 token budget         │
│  - 生成 prompt 上下文        │
└─────────────────────────────┘
  │
  ▼
┌─────────────────────────────┐
│  Expert Team                │
│  - 每个专家拿到自己的上下文  │
│  - 可选工具调用              │
└─────────────────────────────┘
  │
  ▼
┌─────────────────────────────┐
│  Aggregator                 │
│  - 合并 finding              │
│  - 标注 context 来源         │
└─────────────────────────────┘
```

---

## 九、Token 预算控制

仓库上下文很容易超出 token 限制，必须控制：

### 8.1 分层上下文

```rust
pub enum ContextLevel {
    Minimal,     // 只有 diff + 文件列表
    Standard,    // diff + 完整被修改文件 + 相关测试
    Extended,    // diff + 完整文件 + 相关文件 + 相似模式
    Deep,        // 以上 + 调用图 + 历史
}
```

### 8.2 按专家分配上下文

```rust
let context_budgets = HashMap::from([
    ("security", ContextLevel::Extended),
    ("performance", ContextLevel::Standard),
    ("architecture", ContextLevel::Deep),
    ("reuse", ContextLevel::Extended),
    ("testing", ContextLevel::Standard),
]);
```

### 8.3 智能截断

- 大文件只取相关函数 ±50 行
- 相关文件最多取前 N 个
- 相似模式最多取 3 个
- 调用链最多 2 层深度

---

## 十、与全仓库审核的关系

仓库感知型 MR 审核 和 全仓库审核 是互补关系：

| | 仓库感知 MR 审核 | 全仓库审核 |
|--|----------------|-----------|
| 触发 | 每次 MR | 定期/按需 |
| 范围 | MR 相关文件 + 仓库上下文 | 整个仓库 |
| 深度 | 浅-中（验证 MR 相关假设） | 深（系统级诊断） |
| 成本 | 中 | 高 |
| 关系 | 全仓库能力的轻量复用 | 完整仓库健康检查 |

**关键洞察**：

> 仓库感知 MR 审核可以先做，不需要完整建索引。它只需要按需查询仓库文件，就能大幅提升审核质量。

**共享基础设施**：
- `RepoBrowser` 接口
- 文件内容缓存
- 符号索引（如果有）
- 搜索能力

---

## 十一、实施路线图

### Phase 1：基础文件上下文（1-2 周）

**目标**：让 expert 能看到被修改文件的完整内容。

**工作**：
1. 新增 `RepoBrowser` trait，支持 `get_file(path, ref)`
2. GitLab client 增加获取单文件内容 API
3. 本地路径模式：直接读文件系统，支持 `LocalGitBrowser`
4. `ContextAssembler`：把 diff + 完整文件内容打包进 prompt
5. 更新所有 expert 的 prompt，说明有完整文件上下文

**验收**：
- expert 的输出能引用文件中 diff 未展示的行
- 大文件自动截断到相关函数范围
- 本地路径模式下无需调用远程 API 即可完成 review

---

### Phase 2：相关文件与测试（2-3 周）

**目标**：自动找到并加入相关文件和测试。

**工作**：
1. 实现 `get_related_tests(file)` — 同名 `*_test.rs` 或 `tests/` 下文件
2. 实现 `get_imports(file)` — 提取 import/use
3. 实现 `find_related_files_by_imports(file)` — 根据 import 关系找上游/下游
4. `ContextAssembler` 加入 related files
5. 为 Testing 专家专门强化

**验收**：
- 修改 `src/auth/token.rs` 时，自动加入 `tests/auth/token_test.rs`
- Architecture 专家能指出跨文件耦合

---

### Phase 3：模式搜索与复用（3-4 周）

**目标**：让 Reuse 专家能真正发现跨文件重复。

**工作**：
1. 实现 `search_code(pattern)` — 文本/正则搜索
2. 实现 `find_duplicate_blocks()` — 跨文件重复检测
3. 为 Reuse 专家专门设计查询流程
4. 在报告中标注 "similar implementation found in src/xxx.rs"

**验收**：
- 能发现 2 个不同文件中的重复函数
- Riley 的 finding 带具体对比位置

---

### Phase 4：符号级查询（4-6 周）

**目标**：引入 caller/callee 关系。

**工作**：
1. 引入 tree-sitter 解析关键语言
2. 构建轻量符号索引
3. 实现 `find_callers(symbol)` / `find_callees(symbol)`
4. 为 Security / Performance / Architecture 专家启用

**验收**：
- 修改某个函数时，能列出所有调用点
- 能识别未同步更新的调用者

---

### Phase 5：工具调用模式（6-12 周）

**目标**：让 LLM 自主决定查询什么。

**工作**：
1. 设计 tool schema
2. 实现 LLM function calling 循环
3. 让 expert 在需要时主动调用 browser
4. 优化延迟和成本

**验收**：
- expert 遇到不确定问题时能主动查仓库
- 平均每个 MR 的 tool call 次数可控

---

## 十二、下一步建议

如果要立即启动，建议先做 **Phase 1：基础文件上下文**。这是投入最小、收益最大的：

1. 让 GitLab client 支持获取单文件内容
2. 把 diff 中涉及的文件完整内容加入 prompt
3. 调整 expert prompt，强调 "你被允许参考完整文件"
4. 跑几个真实 MR 对比效果

预计 1-2 周内就能看到审核质量的明显提升。
