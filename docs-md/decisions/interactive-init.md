---
title: Interactive init 交互式项目初始化设计
description: review-engine init 从硬编码 TOML 模板改为交互式问答 + inquire 实现
tags:
  - cli
  - init
  - interactive
  - decision
---

# Interactive init 交互式项目初始化设计

## 背景

当前 `review-engine init` 使用硬编码 `toml.push_str(...)` 生成配置文件模板，存在以下问题：

1. 所有参数都写死在代码里，用户无法定制
2. 没有专家选择、权重配置、LLM 启用等交互
3. 输出只能通过 `--output` 重定向，需要用户知道这个 flag

## 决策

将 `init` 改为**交互式问答**模式，用 `inquire` crate 实现终端 UI。

### CLI 变化

```bash
review-engine init            # 交互式问答
review-engine init --default  # 写入内置默认配置到 .code-audit-config.toml
```

去掉 `--output` flag。保存路径在交互中询问。

### 技术选型

使用 `inquire = "0.9"` 而非手写 `print!` + `read_line`，原因：

| 方案 | 优点 | 缺点 |
|------|------|------|
| `inquire` | 多选框、单选列表、输入验证、Y/n 确认、自定义 formatter | 额外依赖 |
| 手写 stdin | 零依赖 | 多选/选择列表需要自行实现，体验差 |

### 交互流程

```
1. 展示检测结果（语言、CI、测试框架）
2. 选择启用哪些命令（MultiSelect）
3. 是否启用 LLM（Confirm）→ 自动检测环境变量
4. 选择参与专家（MultiSelect）
5. 权重分配方式（Select）→ 自动 / 手动
6. 每专家最多发现数（Input）
7. 大 PR 文件阈值（Input）
8. 压缩级别（Select）
9. 保存路径（Input，默认 .code-audit-config.toml）
```

### 数据流

```
用户输入 → inquire → 构建 AppConfig → toml::to_string_pretty → 写入文件
```

### 检测信息展示

在交互开始前，用终端输出显示自动检测结果：

```
╭────────────────────────────────────╮
│  review-engine 项目初始化          │
│  检测到语言: Rust                  │
│  检测到 CI: GitHub Actions         │
│  检测到测试框架: cargo test        │
╰────────────────────────────────────╯
```

### 专家权重分配

选完专家后，有两种权重分配方式：

**自动分配**：按各专家默认权重的比例缩放到 100。

```
选中: lead(20) + security(15) + quality(10) = 45
缩放后: lead=20/45*100≈44, security=15/45*100≈34, quality=10/45*100≈22
```

**手动输入**：逐个专家询问权重值，自动校验总和是否为 100。

### 附录 A：三层配置合并（LLM fallback 机制）

### 问题

项目级 `.code-audit-config.toml` 存在时，用户全局配置 `~/.config/review-engine/.code-audit-config.toml` 完全被忽略。导致全局 `[[llm]]` 无法 fallback，API key 必须写在项目配置中，无法安全上传 git。

### 解决方案：层级合并

```
内置默认（最低优先级）
    ↓ fill：填充空白
用户全局配置 ~/.config/review-engine/.code-audit-config.toml
    ↓ override：覆盖下层
项目配置 .code-audit-config.toml（最高优先级）
    ↓
最终配置
```

```rust
None => {
    // 1. 内置默认 + 环境变量
    let mut config = apply_env_overrides(default_config()?);

    // 2. 用户全局配置（始终加载，填充空白）
    if let Some(user_path) = home_config_path() {
        if user_path.exists() {
            let user_cfg = load_config(user_path)?;
            fill_config(&mut config, user_cfg);
        }
    }

    // 3. 项目配置（有则覆盖）
    if .code-audit-config.toml 存在 {
        let project_cfg = load_config(project_path)?;
        override_config(&mut config, project_cfg);
    }
}
```

### 关键规则

| 场景 | LLM 来源 | API key 是否在项目目录 |
|------|---------|---------------------|
| 项目无 `.code-audit-config.toml` | 全局配置 ✅ | 不在 |
| 项目有 `.code-audit-config.toml`，无 `[[llm]]` | 全局配置 fallback ✅ | 不在（安全上传）|
| 项目有 `.code-audit-config.toml`，有 `[[llm]]` | 项目配置覆盖全局 | 在（用户自愿）|

> **实现状态**：当前实现仅 `[[llm]]` 与 `[report]` 的用户级 fallback（项目配置缺省时回退到用户级 `[[llm]]`，`[report]` 作为全局默认）；上述全字段通用 fill/override 未实现。

### 文件变更

| 文件 | 行数 | 说明 |
|------|------|------|
| `src/config/resolver.rs` | ~30 | `resolve_config` 改为三级合并 |
| `src/config/defaults.rs` | +20 | 新增 `fill_config` / `override_config` |

## 不做的事

| 事项 | 理由 |
|------|------|
| 专家 prompt 编辑 | prompt 很长，不适合终端编辑 |
| `rate_limit` 设置 | 默认值 60rpm / 200k tpm 通用 |
| `scoring.display_*` | 没人会改，默认 true |
| `principles` / `focus` | 已嵌入默认配置 |

## 文件变更

| 文件 | 行数 | 说明 |
|------|------|------|
| `Cargo.toml` | +1 | 加 `inquire` 依赖 |
| `src/actions/init.rs` | ~200 | 重写为交互式 |
| `src/cli/mod.rs` | -3 | 去掉 `Init` 的 `output` 字段 |
| `src/cli/handlers.rs` | ~5 | 简化 handler |
| `docs/decisions/interactive-init.md` | — | 本文 |

## 验收标准

1. `review-engine init` 以一个完整交互流程运行，5-8 步问答后生成配置文件
2. 每步都有默认值，用户可回车跳过
3. 生成的 `.code-audit-config.toml` 可被 `review-engine repo-review --local-path .` 使用
4. `cargo build` + `cargo test` 无 warning 通过
