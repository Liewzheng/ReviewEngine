# ReviewEngine

> 为每个 Pull Request 配备一个虚拟的 **CodeReview 评审委员会** —— 多专家、可打分、可执行。

[![License](https://img.shields.io/badge/License-Apache%202.0-blue.svg)](LICENSE)

**个人免费 · 企业版功能可选**

[English Documentation](README.md)

ReviewEngine 基于 [Apache License 2.0](LICENSE) 发布。核心 CLI、本地评审、GitLab/GitHub 集成、REST API 和默认专家团队均免费开源。SSO、审计日志、自定义专家模板、专属支持等企业功能则通过商业许可单独提供。

---

## 为什么用 ReviewEngine？

持续、深入的代码评审很难做好。团队忙得不可开交，上下文支离破碎，安全漏洞、性能衰退或可复用性机会很容易被忽略 —— 尤其是在大 diff 或不熟悉的代码里。

ReviewEngine 把一支虚拟工程师团队带到每一次评审中：一个可配置的 **CodeReview 评审委员会**，让多位 AI 专家并行审视同一份改动，各自从专业角度出发，输出结构化、可执行的评审意见。

- **多专家并行评审** —— 安全、性能、质量、可复用性、文档等多维度同时评审。
- **结构化输出** —— 每条发现都包含严重级别、置信度、证据、影响、建议和工作量。
- **可打分、可比较** —— 各专家独立打分，再加权汇总为总体得分和明确的风险等级。
- **在你工作的地方运行** —— GitLab MR、GitHub PR、本地仓库、CI/CD 或 REST API。

| 代码评审通常给人的感觉             | ReviewEngine 的做法                                                         |
| ---------------------------------- | ---------------------------------------------------------------------------- |
| "有人检查过 SQL 注入吗？"          | 安全负责人始终在评审委员会里，并明确报告发现。                               |
| "这个 diff 太大了，从哪开始看？"   | 专家各司其职，结果汇总成一份带评分的报告。                                   |
| "我们当初为什么批准了这个改动？"   | 每次评审都有加权得分和可追溯的证据。                                         |
| "又一个要部署维护的工具。"         | 一个静态二进制。`install.sh`、配置、运行。                                   |

---

## 适合谁用？

你可能会喜欢 ReviewEngine，如果你：

- **想要深度，不只是表面评论。** 多专家意味着安全、性能、质量、文档等关注点在单次评审中都能覆盖到。
- **想在提交 PR/MR 之前就完成评审。** 针对 `main`、暂存区或某个提交范围在本地运行，提前修复问题。
- **想要风险信号，而不只是主观意见。** 加权得分和风险等级（低 → 严重）让你更容易判断哪些需要立即处理。
- **想要简单部署。** 一个静态二进制加一份 TOML 配置文件即可开始。

---

## 你能获得什么

|                                        |                                                                                                                |
| -------------------------------------- | -------------------------------------------------------------------------------------------------------------- |
| 🧑‍⚖️ **多专家评审委员会**               | 配置一支 AI 专家团队，各自拥有不同的角色、关注点、原则和权重。                                               |
| 📊 **结构化评分与风险等级**             | 专家独立打分、加权总分，以及风险等级：低 / 低-中 / 中 / 高 / 严重。                                            |
| 💻 **本地优先评审**                     | 支持 `--local-path`、`--base`、`--staged`、`--since`、`--until` —— 不需要远程 MR/PR。                          |
| ⚡ **单一静态二进制**                   | 通过 `install.sh` 安装，可在 CI、笔记本或服务器上任意运行。                                                     |

---

## 快速预览

```markdown
# CodeReview 评审委员会报告

**总体得分：** 72/100  
**风险等级：** 中

---

## 🔴 严重 — 安全负责人

**标题：** 用户输入未经验证直接拼入 SQL 构造器

- **严重级别：** 严重
- **置信度：** 高
- **工作量：** 中
- **影响：** 可能导致 SQL 注入，造成未授权数据访问
- **证据：** `src/db.rs:42` —— 直接执行 `query.push_str(&user_input)`，未做参数化处理
- **建议：** 使用参数化查询或预编译语句构造器。补充集成测试，可用 sqlmap 或同类工具。

---

## 🟠 高 — 性能负责人

**标题：** 热路径中存在对无索引关系的嵌套循环

- **严重级别：** 高
- **置信度：** 中
- **工作量：** 低
- **影响：** 负载下呈 O(n²) 行为；数据量超过 1 万行时可能出现延迟尖峰
- **证据：** `src/search.rs:88` —— `load_user_records()` 函数内的循环
- **建议：** 在 `user_id` 上添加数据库索引，并考虑批量读取。

---

## 专家得分

| 专家                 | 得分   | 权重   |
| -------------------- | ------ | ------ |
| 安全负责人           | 45/100 | 25%    |
| 性能负责人           | 70/100 | 20%    |
| 质量负责人           | 85/100 | 20%    |
| 可复用性负责人       | 80/100 | 15%    |
| 文档负责人           | 90/100 | 10%    |
| 可维护性负责人       | 78/100 | 10%    |
```

---

## 快速开始

安装最新静态二进制：

```bash
curl -fsSL https://raw.githubusercontent.com/Liewzheng/Review-Engine/master/install.sh | bash
```

安装脚本依赖 `curl`、`jq` 和 `sha256sum`（Linux）或 `shasum`（macOS）。

> **安全提示：** 你也可以先下载脚本、检查内容后再本地运行：
> ```bash
> curl -fsSL https://raw.githubusercontent.com/Liewzheng/Review-Engine/master/install.sh -o install.sh
> # 检查 install.sh 内容，然后：
> bash install.sh
> ```

配置 LLM 供应商。DeepSeek 通过 OpenAI 兼容 API 访问：

```bash
export LLM_CONFIG='[{"provider":"openai","model":"deepseek-chat","api_key":"sk-your-key","api_base":"https://api.deepseek.com/v1","max_tokens":4096,"temperature":0.3}]'
```

或者运行 `review-engine init` 为当前项目生成 `.code-audit-config.toml`。

运行第一次本地评审：

```bash
review-engine review --local-path . --base main
```

更详细的入门指南见 [`docs/getting-started.md`](docs/getting-started.md)。  
完整的 CLI 选项、环境变量、LLM 供应商和配置参考见 [`docs/configuration.md`](docs/configuration.md)、[`docs/integrations/`](docs/integrations/) 和 [`docs/rest-api.md`](docs/rest-api.md)。

### 常用选项

| 选项 | 说明 |
|---|---|
| `--local-path <path>` | 要评审的仓库路径。 |
| `--base <ref>` | 对比的基准 ref（例如 `main`）。 |
| `--staged` | 只评审暂存区改动。 |
| `--since <ref>` / `--until <ref>` | 评审一个提交范围。 |
| `--format <json 或 markdown>` | 输出格式。 |
| `--output <file>` | 将报告写入文件。 |
| `--publish` | 将评审结果发布回 MR/PR 讨论区。 |

---

## 支持的 LLM 供应商

ReviewEngine 开箱即用支持多种 LLM 供应商：

- **OpenAI**（例如 GPT-4o）
- **Anthropic**（例如 Claude）
- **DeepSeek**
- **任何兼容 OpenAI 协议的供应商**

在 `.code-audit-config.toml` 中或通过 `LLM_CONFIG` 环境变量配置供应商：

```toml
[[llm]]
provider = "openai"
model = "gpt-4o"
api_key = "sk-your-key"
api_base = "https://api.openai.com/v1"
max_tokens = 4096
temperature = 0.3
```

> **安全提示**：将 `sk-your-key` 替换为真实密钥，并通过 `LLM_CONFIG` 环境变量或密钥管理工具在运行时注入。请勿将真实凭据提交到版本控制。

完整配置参考见 [`docs/configuration.md`](docs/configuration.md)。

---

## 集成方式

ReviewEngine 可通过多种入口融入现有工作流：

- **GitLab MR** —— 通过 CLI 使用 `--mr-url`，或通过 webhook 评论命令（`/review`、`/improve`）。
- **GitHub PR** —— 通过 CLI 使用 `--mr-url` 或 webhook。
- **本地仓库** —— 对工作区、暂存区或提交范围进行评审，无需远程 MR/PR。
- **CI/CD** —— 作为 GitLab CI、GitHub Actions 或任意流水线的一个步骤运行。
- **REST API** —— 启动 `review-engine serve`，通过 HTTP 触发评审。

```bash
# GitLab MR 评审（在环境变量中设置 GITLAB_TOKEN）
review-engine review --mr-url https://gitlab.com/owner/repo/-/merge_requests/42

# GitHub PR 评审（在环境变量中设置 GITHUB_TOKEN）
review-engine review --mr-url https://github.com/owner/repo/pull/123

# 启动 REST / webhook 服务
review-engine serve --port 8080
```

> **安全提示：** 通过 `GITLAB_TOKEN` 和 `GITHUB_TOKEN` 环境变量传递 token，避免使用 `--gitlab-token` / `--github-token` 命令行参数，防止 token 泄漏到 shell 历史或进程列表中。

更多示例和配置指南见 [`docs/integrations/`](docs/integrations/)。

---

## AI 技能

ReviewEngine 也可以作为兼容 [Agent Skills](https://github.com/cline/agent-skills) 的 AI skill 使用，从而直接在支持的 Agent 中触发评审。

支持的 Agent 包括 **Kimi Code**、**Claude Code**、**Codex CLI**、**OpenCode**、**Cursor** 以及其他兼容 Agent Skills 的客户端。

全局安装该 skill：

```bash
cp -R .kimi-code/skills/review-engine ~/.kimi-code/skills/
# 对于 Claude Code：~/.claude/skills/
```

安装后，使用 **"review-engine"**、**"review this repo"**、**"repo review"** 或 **"review a PR"** 等短语触发。

详情见 [`.kimi-code/skills/review-engine/SKILL.md`](.kimi-code/skills/review-engine/SKILL.md)。

---

## 架构

```
输入 → 配置解析 → 专家选择 → 并行评审 → 结果汇总 → 评分报告
```

- 使用 **Rust** 构建，启动快、并发可靠。
- 通过 `install.sh` 分发为**单一静态二进制**。
- 专家团队通过 `.code-audit-config.toml` 配置驱动。
- 并行调用 LLM，每个专家拥有独立的提示词、权重和关注点。
- 可选 **REST API**（`review-engine serve`）支持 webhook 和前端集成。
- 可选 **全仓库健康检查**（`review-engine repo-review`）用于更广泛的代码库分析。

---

## 性能

ReviewEngine 设计为轻量且适合 CI 运行。资源消耗主要由 LLM 网络延迟决定，而不是本地 CPU 或内存。

针对约 3 万行代码仓库的基准测试（3 次运行，`repo-review`，本地 CLI，DeepSeek 模型）：

| 指标 | 平均值 |
|---|---|
| 总耗时 | 约 5 分 46 秒 |
| 峰值内存 | 约 9 MB |
| Max RSS | 约 19 MB |
| CPU 时间 | 约 0.07 秒 |

对于典型的 branch/MR 评审，`review` 命令通常在 **30–50 秒** 内完成，具体取决于 LLM 提供商和网络状况。

---

## 命令

ReviewEngine 围绕一组精简的命令组织：

| 命令                 | 用途                                                       |
| -------------------- | ---------------------------------------------------------- |
| `review`             | 对 MR、PR 或本地 diff 运行 CodeReview 评审委员会。         |
| `describe`           | 从 diff 生成摘要或 MR/PR 描述。                            |
| `improve`            | 为 diff 提供具体代码改进建议。                             |
| `repo-review`        | 对整个代码库进行全仓库健康检查。                           |
| `update_changelog`   | 根据近期提交生成或更新变更日志。                           |
| `serve`              | 启动 REST API 和 webhook 服务。                            |
| `validate`           | 校验你的 `.code-audit-config.toml`。                       |
| `init`               | 为新项目生成一份初始配置。                                 |
| `default`            | 打印内置默认配置。                                         |
| `generate-token`     | 为 `review-engine serve` 生成随机 API token。              |

---

## 路线图与社区

- 近期发布和路线图里程碑见 [`CHANGELOG.md`](CHANGELOG.md)。
- 如何贡献代码、文档或 issue 见 [`CONTRIBUTING.md`](CONTRIBUTING.md)。

我们欢迎贡献者。无论是 bug 报告、文档改进，还是新的专家想法，都欢迎提交 issue 或 pull request。

---

## 企业版

- **核心版** —— Apache-2.0 协议，个人和团队免费，在本仓库中开发。
- **企业版** —— 单独提供 SSO、审计日志、自定义专家模板、高级分析和专属支持。

企业版详情请见 [`docs/enterprise.md`](docs/enterprise.md)。

---

## 许可

ReviewEngine 核心基于 [Apache License 2.0](LICENSE) 许可。企业版功能和商业支持单独开发，不属于本开源仓库的一部分。
