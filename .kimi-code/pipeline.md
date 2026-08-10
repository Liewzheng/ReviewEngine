# ReviewEngine 项目工作流（pipeline）

## 并行派工用 git worktree 隔离（2026-08-10 用户决策）

**背景**：2026-08-10 事故——多个并行 agent 共用同一工作区时，一次 `git checkout -B` 分支 reset 导致两个已验收但未推送的提交（e6abc6f/ae530fb）悬空丢失（后从 reflog 救回）。共享工作区是干扰根因。

**规则**：
- 派 2 个及以上**会改代码**的并行 agent 时，**每个 agent 给独立 worktree 隔离工作区**，不共享主 checkout：
  ```bash
  git worktree add .worktrees/<task> -b <agent-branch> main
  # 派工时在 prompt 里指定该 agent 只在该 worktree 目录工作
  ```
- 各 agent 在自己的 worktree + 自己的分支上干活、自行 commit；主 agent 验收后负责把各分支合并回集成分支（`feat/backend-frontend-integration`）。
- **主 agent 绝不**在有在途工作的主工作区上执行 `checkout -B` / `reset --hard` / `clean` 等会改动工作区或分支指针的操作；如需 reset，先确认所有提交已推送或已并入集成分支。
- 单 agent 执行任务（或纯只读调查）可直接用主工作区，不必开 worktree。
- 合并完成后用 `git worktree remove .worktrees/<task>` 清理，并删除对应临时分支。
- 例外：多 agent 改**完全不相交**的文件且都明确不 commit（只留工作区改动由主 agent 统一提交）时，可共享主工作区——但优先 worktree。

## 其他既定流程
- 每轮改动跑自审查（`reng review/audit --progress`）+ CI 全绿才合并；合并走 squash。
- 发版：tag → release.yml 12 资产 → `update-formula.yml`（tap）→ brew upgrade → 冒烟。
- 测试用例：两阶段派工法（写预期与实测分离），CSV 存 git 跟踪的 `tests/cases/`、带版本号。
- 派工模型：默认 deepseek-v4-flash；视觉/UI 验证派 kimi-code/k3；不用本地模型。
