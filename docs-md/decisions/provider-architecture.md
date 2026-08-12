---
title: Provider 架构决策
description: Git 平台 Provider（GitLab、GitHub、Bitbucket 等）的抽象层次设计原则
tags:
  - architecture
  - git-provider
  - decision
related:
  - ../../src/git_provider/mod.rs
  - ../../src/git_provider/gitlab.rs
  - ../../src/git_provider/github.rs
---

# Provider 架构决策

## 背景

review-engine 需要支持多个 Git 平台（GitLab、GitHub 等）。每个平台都有不同的 REST API、认证方式、URL 模式和响应结构。

当前已有：
- `GitLabProvider` — 基于 GitLab REST API（`/api/v4/projects/...`）
- `GitHubProvider` — 基于 GitHub REST API（`/api/v3/repos/...`）

## 问题

两个 provider 在 HTTP 请求层有部分相似模式（状态码检查、JSON 解析、错误处理）。是否应该抽取一个统一的 HTTP 客户端层来消除重复？

## 决策

**不抽取统一 HTTP 层。抽象只在 trait 方法层，不在 HTTP 请求层。**

### 理由

1. **每个平台的 API 差异大于共性**
   - URL 结构完全不同（`/api/v4/projects/{id}` vs `/repos/{owner}/{repo}`）
   - 认证方式不同（GitLab 支持 `PRIVATE-TOKEN`/`Bearer`，GitHub 必须 `Bearer` + `User-Agent`）
   - 响应结构不同（字段名、嵌套层级、分页机制）
   - 错误语义不同（状态码含义、限流格式）

2. **统一 HTTP 层会引入额外的复杂性**
   - 需要抽象 URL 构建、认证注入、错误映射
   - 每个新 provider 仍需要适配这个抽象层
   - 抽象不当反而增加维护成本

3. **Rust 没有成熟的 Git 平台 SDK**
   - Python 有 PyGithub、python-gitlab 等官方 SDK
   - Rust 生态中没有等价物，必须自己写 HTTP 客户端

### 抽象层次

```
┌─────────────────────────────────────────────┐
│  GitProvider trait                           │
│  ├── fetch_mr_info() → MRInfo               │
│  ├── fetch_diff() → String                   │
│  ├── post_review_comment()                   │
│  ├── post_inline_comment()                   │
│  ├── fetch_code_audit_toml()                │
│  ├── add_reaction()                         │
│  ├── find_or_update_discussion()            │
│  └── update_discussion()                    │
├─────────────────────────────────────────────┤
│  GitLabProvider          GitHubProvider      │
│  (reqwest + 自有 client)  (reqwest + 自有    │
│                           client)            │
├─────────────────────────────────────────────┤
│  未来: BitbucketProvider  GiteeProvider ...  │
│  (各用各的 reqwest client)                   │
└─────────────────────────────────────────────┘
```

方法语义补充（v0.7.10 同步）：

- `post_review_comment()` — 新建一条顶层评论，用于发布完整评审报告。
- `post_inline_comment()` — 在 diff 的特定行上发内联评论。
- `find_or_update_discussion()` — upsert 语义：查找本工具已创建的讨论（通过标记识别），存在则更新其内容，不存在则新建。用于 webhook 重发时避免刷屏式重复评论。
- `update_discussion()` — 全量替换指定讨论的内容，要求讨论已存在；是 `find_or_update_discussion` 默认实现内部使用的基础操作。

> 注：早期设计图中的 `supported_capabilities()` 从未在代码中存在（`src/git_provider/mod.rs` 的 trait 无此方法），已从本文档移除，不涉及任何代码破坏性变更。

### 可接受的重复

两个 provider 在以下方面可以有合理重复：
- HTTP 请求的状态码检查 + JSON 解析（约每方法 3-5 行）
- `Debug` 实现（隐藏 token）
- API URL 拼接

这些重复是 **有意的**——每个 provider 需要独立控制自己的请求细节。当 provider 数量增加时，如果某种模式被证明完全一致，再考虑提取公共辅助函数，而非完整的 HTTP 抽象。

## 何时重新评估

当满足以下条件时，可以重新考虑 HTTP 层抽象：
1. provider 数量 ≥ 5
2. 至少有 3 个 provider 使用了完全相同的错误处理模式
3. 新增 provider 时 HTTP 代码的复制成本明显高于抽象维护成本

当前（2026-06）不满足任何一个条件。
