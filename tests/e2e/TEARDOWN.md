# E2E Teardown 契约(v0.9.50 起生效)

E2E 测试的交付物不只是「测过」,还包括「场地恢复原样」。本契约规定每轮 E2E 结束后的清理义务,配套脚本为 `tests/e2e/teardown.sh`。

## 背景:为什么需要这份契约

0.9.49 及之前的 6 轮 E2E(round-1 ~ round-6)都没有 teardown,导致:

- `review-lab/e2e-security` 堆积 6 个 open MR、6 个 `e2e/*` 测试分支;
- root 下残留 3 个未撤销的临时 PAT(`e2e-round4-auto` / `e2e-round5-agent` / `e2e-round6-accept`),其中一个明文嵌在克隆的 remote URL 里;
- `/tmp/` 下残留多个 git 克隆;浏览器(webbridge)残留 10 个测试标签 session 共 15 个标签页。

上述残留已于 2026-09-02 人工清理(见 `reports/test-cases/e2e-teardown-v0.9.50.md` 实测记录)。本契约把这种清理固化为每轮 E2E 的强制步骤,防止再次堆积。

## 契约条款

### 1. 何时必须跑

- **每轮 E2E 测试结束时**,无论通过/失败/中途放弃,都必须运行 `tests/e2e/teardown.sh`。
- CI/定时任务若跑 E2E,teardown 必须放在 `finally` 等价位置,测试失败不豁免清理。
- 判断标准:跑完后 `teardown.sh` 的 dry-run 应为 no-op(动作数 0、退出码 0)。

### 2. 跑什么

```bash
tests/e2e/teardown.sh          # 先 dry-run:核对将清理的对象是否符合预期
tests/e2e/teardown.sh --yes    # 确认后实跑
```

脚本覆盖 5 项清理(全部为幂等操作):

| # | 目标 | 动作 |
| --- | --- | --- |
| 1 | `gitlab-ee-testbed` 容器内 `review-lab/e2e-security` | 关闭所有 open MR(不 merge) |
| 2 | 同上 | 删除所有 `e2e/*` 测试分支 |
| 3 | 同上 | revoke 所有 `e2e-*` / `*accept*` 命名的临时 PAT |
| 4 | 本机 `/tmp/e2e-*` | 删除核实过的测试克隆(是 git repo 且 origin 指向测试床) |
| 5 | webbridge daemon(127.0.0.1:10086) | 关闭所有标签均为测试页的 session |

脚本特性:默认 dry-run;任一步失败不中断后续;退出码 = 失败步骤数;顶部集中配置(容器名/项目路径/匹配模式),换环境只改配置块。

### 3. 红线(脚本与人工操作共同遵守)

- **不删除 `review-lab/e2e-security` 项目本体** —— 它是保留的固定回归测试床;
- **不动 `main` 及非 `e2e/*` 分支**;
- **不动 root 长期 token**(`review-engine-test-token`,白名单见脚本 `KEEP_TOKEN_NAMES`);
- **不动容器配置**:`gitlab-ee-testbed` 只允许 `gitlab-rails runner` 读写业务数据,`review-engine-preview`(18080)完全不碰;
- **不动用户自己的浏览器标签**:webbridge session 里混入非测试标签时脚本跳过并告警,交人工核对,绝不强制关。

### 4. 与测试用例的挂钩方式

- 每个 E2E 测试计划(测试用例文档)末尾必须列出 teardown 步骤,状态随本轮测试一并记录;
- teardown 本身登记为固定回归用例 **E2E-TD-001**(前置条件/步骤/预期/实测记录见 `reports/test-cases/e2e-teardown-v0.9.50.md`);
- 每轮 E2E 报告中,「teardown 复核」一节粘贴最后一次 dry-run 输出作为场地已恢复的证据(动作数应为 0)。

## 环境适配备注(2026-09-02 实测)

- 该 GitLab 版本 `merge_requests.state` 列已迁移为 `state_id`,直接 `where(state:)` 会 `PG::UndefinedColumn`,须用 `.opened` scope;
- `ServiceResponse` 无 `#error` 方法,错误信息取 `#message`;`Files::CreateService#execute` 在该版本返回 Hash(`:status` 键)而非 ServiceResponse;
- webbridge daemon 无 groups / session-list API,session 名需从 `~/.kimi-webbridge/logs/daemon.log` 的历史记录提取后逐一 `list_tabs` 探测;
- E2E 脚本不应把 PAT 明文嵌入克隆的 remote URL(0.9.49 前各轮的残留均如此);若必须,teardown 的 clone 删除 + PAT revoke 是唯一兜底。
