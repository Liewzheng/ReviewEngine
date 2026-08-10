# tests/cases — 预期行为测试规格

本目录存放「预期行为测试规格」（expected-behavior test spec）CSV，供有视觉能力的测试工程师用真实浏览器（webbridge）逐条遍历核对判定。

## 文件清单

| 文件 | 编写人 | 状态 |
| --- | --- | --- |
| `ui-test-cases-v0.9.4.csv` | 沈一帆（前端，从代码推导） | 已就绪 |
| `cli-test-cases-v0.9.4.csv` | 文慧（CLI，从 CLI 推导） | 已就绪（135 条，格式：`用例编号,子命令,参数/输入组合,预期输出或行为,判定(是/否),实测输出,备注`） |

> 本 README 由沈一帆创建，覆盖 UI 规格；文慧的 CLI 规格未另建 README，需要时可并入本文件。

## 对应代码版本

- **基线**：tag `v0.9.4`（2026-08-07），分支 `feat/backend-frontend-integration`。
- **预期以工作树为准**：本 CSV 的部分用例标注了「预期修复后行为」，依据的是 `v0.9.4` 发版在途的未提交修复 diff：
  - `frontend/src/views/Configuration.vue` — 保存死结修复（空 URL/Token/Key 不再 required、requiredExperts 空值回填、provider-only 保存分支）；
  - `frontend/src/views/QueueMonitor.vue` — Max Concurrent 步进器提交修复（`@update:model-value` 400ms 防抖提交）；
  - `frontend/src/services/logs.ts` — SSE 日志流 token 走 `?token=` 查询串。
- 实测时若这些修复未合入，标注「预期修复后行为」的用例应判「否」并记录实际表现。

## 用法（谁写预期、谁实测、判定如何填）

1. **谁写预期**：前端工程师（沈一帆）从 `frontend/src/views/*.vue`、`components/**`、`router/index.ts`、`services/*.ts`、`composables/*.ts` 推导「代码应有的行为」，写入「预期布局或结果」列。不运行、不实测。
2. **谁实测**：有视觉能力的测试工程师（k3）用真实浏览器（webbridge）打开 `http://<host>/#/...` 逐条遍历。
3. **判定如何填**：
   - 「判定(是/否)」：预期与实际一致填「是」，不一致填「否」；无法验证（如依赖真实 GitLab MR）填「-」并在备注说明。
   - 「实际结果」：记录浏览器中实际观察到的行为（含截图/API 面板证据），简明扼要。
   - 每条至少填判定列；「否」必须填实际结果。
4. 编号前缀 = 页面分组（G 全局框架 / D Dashboard / H ReviewHistory / Q QueueMonitor / C Configuration / E ExpertsManagement / L LlmStatus / S SystemLogs / U Upgrade 弹窗），后缀为序号。末尾 STAT-* 行为覆盖统计，不参与实测。

## 标注约定

- **【已知 bug 预期修复后】**：该交互在 v0.9.3/0.9.4 正在修复（保存死结、删除后保存、Test Connection 400、Max Concurrent 步进器）；预期行为 = 修复后的行为，实测时重点核对是否已修复。
- **【推导存疑】**：代码行为与直觉/需求可能不符或为占位实现，实测时重点核对（见下）。

## 推导存疑清单（供测试者重点核对）

1. **H-01 ReviewHistory「Export」按钮无任何 `@click` 处理器**：点击预期无动作（可能为未实现功能或遗漏绑定）。
2. **Q-13 QueueMonitor 任务卡「Logs」**：`handleViewLogs` 仅为 info 通知占位，不跳转日志页。
3. **L-08 LlmStatus「Configure」按钮**：跳转 `/config?tab=llm&provider={id}`，但 `Configuration.vue` 不读取 `route.query.tab/provider`，跳转后无定位/预选效果。
4. **G-13 / G-14 无 token 时 Cancel 关闭弹窗后**：API 请求将 401，各页表现为空态 + 错误通知（各页错误态用例应能观察到，勿误判为页面崩溃）。
5. **D-12 自动刷新**：Dashboard 组件级（60s）+ composable 级（60s）各有一个定时器，实测可观察到约每 60s 一次 `/dashboard` 请求。
6. **C-12 保存死结（修复中）**：修复前全新空配置无法保存（URL/Token/Key required 校验），修复后应可保存；若实测「Save 一直禁用」即未修复。
7. **C-15 Test Connection 400（修复中）**：修复前配置不完整时后端 400 导致结果展示异常，修复后应落入「Failed — ...」红色 tag。
8. **C-18 删除后保存（修复中）**：删除 provider 后保存应发 `DELETE /api/v1/llm/providers/{id}`，404 幂等放行；若刷新后 provider 仍存在即未修复。
9. **Q-10 Max Concurrent 步进器（修复中）**：修复前 +/- 步进无提交，修复后 400ms 防抖提交一次请求。

## 覆盖统计（v0.9.4，共 136 条 = 布局 56 + 交互 80）

| 页面 | 布局 | 交互 | 小计 |
| --- | --- | --- | --- |
| 全局框架（G） | 7 | 8 | 15 |
| Dashboard（D） | 7 | 7 | 14 |
| ReviewHistory（H） | 7 | 12 | 19 |
| QueueMonitor（Q） | 7 | 10 | 17 |
| Configuration（C） | 8 | 15 | 23 |
| ExpertsManagement（E） | 7 | 9 | 16 |
| LlmStatus（L） | 4 | 6 | 10 |
| SystemLogs（S） | 5 | 9 | 14 |
| Upgrade 弹窗（U） | 4 | 4 | 8 |
| **合计** | **56** | **80** | **136** |

## CSV 格式说明

- UTF-8 含表头，8 列：`用例编号,页面,类型(布局/交互),操作或位置,预期布局或结果,判定(是/否),实际结果,备注`。
- 字段含逗号/换行/引号时按 RFC 4180 用双引号包裹并转义（本文件由 `csv.QUOTE_ALL` 生成，全部字段带引号，Excel / 文本编辑器均可直接打开）。
- 判定与实际结果两列留空，由实测者填写。
