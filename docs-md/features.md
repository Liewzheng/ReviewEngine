# review-engine 特性概览

> 本文为宣传向特性文档，以中文撰写；如需要，可提供英文版（English version available on request）。
> 文中性能与资源数据为 **v0.9.14 四平台实测（2026-08-12：macOS arm64 / Linux x86_64 / Linux aarch64 / Windows x86_64）**，分「通俗主文」与「技术附录」两部分——技术附录含每项指标的测量方法与来源，可复核。2026-08-03 旧版本基线（v0.9.x 早期）及早期小 diff 数据单独标注、不参与对比。只陈述事实，不夸大。

## 概述

review-engine 是一个用 Rust 编写的代码评审引擎：它以「多位 AI 专家组成的虚拟团队」为模型，对代码仓库或分支差异进行多角度评审，输出结构化报告，并可直接回贴到 GitLab MR / GitHub PR 讨论区。

它同时提供 **命令行（CLI）** 与 **Web 管理界面（Server）** 两种形态：CLI 负责本地评审、审计与自升级；Server 提供 REST API 与前端，便于团队集中部署、查看评审历史与配置管理。整个核心由单一静态二进制承载，安装、升级、跨架构部署都异常轻快。

一句话概括：**把一支随时可用的 AI 评审团队，装进一个只有几十 MiB 常驻内存的二进制里。**

## 核心特性

### 双命令设计：`reng` / `review-engine`

同一份二进制，两个名字。无论通过 Homebrew tap、`install.sh` 还是 Docker 镜像安装，都会自带 `reng` 短别名；命令名随调用方式自适应显示——你用 `reng` 调用，帮助与用法输出就显示 `reng`，用 `review-engine` 调用则显示全名。短、好记、不用纠结别名配置。

### 分工清晰的 CLI：`audit` 与 `review`

命令行围绕两种真实场景做了明确分工：

- **`review`** —— 分支评审：评审当前分支相对基线的差异、暂存区改动或指定 commit 区间，聚焦「这次改动会引入什么问题」。
- **`audit`**（即 `repo-review` 的别名）—— 整仓审计：对全仓库做一轮健康度体检，输出整体评审结论，适合定期巡检。

```bash
reng review --local-path . --base main            # 评审当前分支改动
reng audit --local-path . --format markdown       # 整仓健康审计
```

### CLI 自升级：`reng upgrade`

升级不再依赖外部脚本。内置的 `reng upgrade` 走一条完整可信的链路：

1. 查询 GitHub Releases，定位最新稳定版与当前平台对应的发布资产；
2. 下载并做 **sha256 双重校验**（发布资产 + 官方 checksum 侧车文件）；
3. 安全解压——内置对 **zip-slip 路径逃逸** 与 **解压炸弹** 的防护；
4. 替换前自动备份原二进制，替换后执行冒烟检查，失败则**自动回滚**。

更贴心的是，它会自动识别你的安装方式并给出对应升级命令：Homebrew 安装提示 `brew upgrade review-engine`，cargo 安装提示重新 `cargo install`，Docker 环境提示在容器内自动升级（完成后容器自动重启），普通二进制则直接 `reng upgrade`。提示样式参照知名 CLI 工具，一眼就知道该做什么。

### Web 升级：一个弹窗，完成更新

部署为 Server 形态时，升级全流程都收进了浏览器：

- `GET /api/v1/system/upgrade/check` —— 检查最新版本（服务端 **1 小时缓存**，不反复打 GitHub API）；
- `POST /api/v1/system/upgrade` —— 发起升级（**单飞**：同一时刻只允许一个升级任务，进行中并发请求直接 409 拒绝）；
- `GET /api/v1/system/upgrade/status` —— 查询进度（**8 态状态机**：空闲 / 检查 / 下载 / 校验 / 安装 / 完成 / 失败 / 不支持）。

前端顶部有版本 chip，检测到新版本时亮起 **Update available** 标签并弹出升级窗口；Docker 形态支持**容器内自动升级**——点 Upgrade 即可替换二进制与前端并自动重启，无需重建镜像或进容器。

### API 鉴权：开箱即用，不裸奔

REST API 采用 **Bearer Token 与 X-API-Key 双通道** 鉴权，比较使用**恒定时间比较**（constant-time），从时序上抵抗令牌猜测。默认只绑定回环地址；一旦绑定非回环地址，**必须配置 token，否则服务拒绝启动**——开启鉴权后，无 token 或 token 错误的请求一律返回 401。默认安全，不是事后补丁。

### 前端性能：首屏快到没有存在感

前端采用**路由级懒加载** + **element-plus 按需引入**，只打包真正用到的组件与样式。element-plus 单 chunk gzip 后仅 **166 kB**，首屏体积相比全量引入大幅下降——页面打开速度，配得上引擎本身的轻快。

## 实测数据

> 性能数据分两部分：**通俗主文**（写给部署小白：结论 + 类比 + 核心指标）与**技术附录**（写给技术复核者：统一基线、四平台完整数据、每项指标的测量方法与来源）。所有数据均为 **v0.9.14** 实测（2026-08-12）。

### 通俗主文：一句话结论 + 三个类比 + 核心指标

**结论先行：一台 2 GB 内存的小 NAS 就能跑。** 空载只占十几 MB 内存（大约一个浏览器标签页），评审时 CPU 几乎不忙——99% 以上的时间在等 AI 回复，真正的成本是等待时长，不是机器性能。

三个类比，好记：

- **内存 ≈ 一个浏览器标签页**：空载 8.6–20.5 MiB，评审峰值也不到 56 MiB——比你开着的这个页面还轻；
- **磁盘体积 ≈ 一部小电影**：CLI 二进制 13–18 MiB，Docker 镜像 62.9–282 MB（视平台与口径），任何 NAS 都塞得下；
- **评审 ≈ 等 AI 打字**：一轮 11 位 AI 专家的完整评审，本机只忙 0.11–0.63 秒，其余 99% 以上的时间都在等外部模型回复——墙钟 50–108 秒，主要花在网络和 AI 出字速度上。

核心指标（四平台，v0.9.14 实测）：

| 指标 | 数值 | 一句话 |
|---|---|---|
| 常驻内存 | 空载 8.6–20.5 MiB；评审峰值 42–56 MiB | 一个浏览器标签页级别 |
| 磁盘体积 | CLI 二进制 13–18 MiB；Docker 镜像 62.9–282 MB | 一部小电影级别 |
| 评审墙钟 | 直连约 1–2 分钟（快网络 50 s；Linux 直连约 97–108 s） | 等 AI 打字的时间 |
| CPU 平均占用 | **<1%**（0.10–0.63%） | 本机几乎不忙 |

冷启动一句：命令启动毫秒级——Linux/macOS 6–8 ms，Windows 36–38 ms，每次调用都是"瞬开"。

部署场景建议：

- **2 GB 内存的小 NAS 能跑**：空载 <21 MiB、评审峰值 <56 MiB，远低于 2 GB 预算；镜像最大 282 MB，磁盘无压力；
- **想让评审快一点，加内存没用**：瓶颈在网络 RTT 与 LLM 延迟（见技术附录 D「直连 vs 代理」）。收益最大的是网络路径（靠近 API、少代理中转），其次才是机器；
- **Windows 与 Linux/macOS 资源表现同级**：峰值 RSS 46 MiB、空载 12.6 MiB；冷启动 36–38 ms 是 Windows 平台开销，不影响部署选择。

### 技术附录：统一基线 + 四平台完整数据 + 测量方法

#### A. 统一基线声明（保证四平台可比）

- **版本**：v0.9.14（macOS 为 cargo release 构建；Linux x86_64 / Windows 为官方 release 资产并 sha256 校验；Linux aarch64 因官方资产依赖 GLIBC_2.39、目标机为 glibc 2.35，改用同 tag 源码 `cargo build --release`）；
- **模型**：deepseek-v4-flash（max_tokens 4096，temperature 0.3）；
- **评审对象**：标准对象 perf-bench-repo —— 20 个源文件 / 2000 行 / 10 个文件改动（+150/−40）/ 估算输入 token ≈10.4k（(diff 文本 11,606 B + 改动内容 24,828 B) ÷ 3.5，不含模板）；
- **对象一致性铁证**：macOS（本机生成）与 Linux 两机（同一 tarball 部署）的 `HEAD~1^{tree}=f473ed1c`、`HEAD^{tree}=adcb8901` **完全一致**——git tree hash 只取决于内容，证明三平台评审的是字节级相同的对象；
- **网络**：以直连 api.deepseek.com 为主（Windows 二进制无系统代理特性，天然直连；Linux 两机额外测代理对照，见 D）；
- **评审命令**：`review-engine review --local-path perf-bench-repo --base HEAD~1`（11 位 AI 专家，每专家 1 次 LLM 调用/任务）。

#### B. 四平台完整对比表（统一对象，直连为主）

| 指标 | macOS arm64 | Linux x86_64 | Linux aarch64 | Windows x86_64 |
|---|---|---|---|---|
| 墙钟 (s) | 50.02 | 107.61（直连）/ 114.86（代理） | 97.07（直连）/ 112.33（代理） | 102.45 |
| CPU 总时间 (s) | 0.240 | 0.110 / 0.120 | 0.615 / 0.625 | 0.391 |
| CPU 平均占用率 | 0.48% | 0.10% / 0.10% | 0.63% / 0.56% | 0.38% |
| LLM 等待占比 | 99.52% | 99.90% / 99.90% | 99.37% / 99.44% | >99.6% |
| 峰值 RSS (MiB) | 55.2 | 44.2 / 44.1 | 42.5 / 42.1 | 46.2 |
| 空载 RSS (MiB) | 20.5 | 11.5 | 8.6 | 12.6 |
| 冷启动 `--version` (ms) | 8.3 | 6.0 | 8.0 | 36.2（min）/ 37.6（median） |
| 镜像体积 | 62.9 MB content / 282 MB disk | 168 MB（disk） | 231 MB（disk） | N/A（无 Docker） |

> Windows 说明：同规格复用（20 文件 / 2000 行 / +150/−40），但本机为 8 种语言构成、diff 文本 19,978 B（三平台为 5 种语言、11,606 B）——**资源指标（CPU / RSS / 冷启动）可比**，墙钟含对象差异，严格横向对比需同一 tarball 重跑。另：macOS / Linux 本地镜像内嵌二进制为 v0.9.12 / v0.9.6（落后源码一至两版），镜像体积数据来自这些镜像；CPU / 内存数据来自 v0.9.14 二进制。

#### C. 每项指标的计算方法与来源（逐项）

| 指标 | 计算方法 | 测量工具与来源 |
|---|---|---|
| 墙钟 | 定义 = 评审进程结束时间 − 开始时间 | macOS / Linux x86_64：`/usr/bin/time -l` / `-v` 的 `real`（objective 报告 §3：macOS real 50.02、lab 107.61/114.86）；Linux aarch64：perf_counter / 日志时间戳（97.07 / 112.33）；Windows：脚本 start/end 差分（review_wallclock_s=102.45）。样本数：每平台 1 次真实付费评审（11 专家全流程） |
| CPU 总时间 | 定义 = user + sys（秒） | macOS / Linux x86_64：`/usr/bin/time` 记账（macOS 0.14+0.10=0.240；lab 0.11/0.12）；Linux aarch64：`getrusage(RUSAGE_CHILDREN)` 的 ru_utime + ru_stime（含已回收的 git 子进程，语义一致；0.615/0.625）；Windows：`Get-Process .TotalProcessorTime` 差分（0.391 s） |
| CPU 平均占用率 | 公式 = CPU 总时间 ÷ 墙钟 × 100% | 由本表「CPU 总时间」÷「墙钟」（macOS 0.240/50.02=0.48%；lab 0.110/107.61=0.10%；orangepi 0.615/97.07=0.63%；Windows 0.391/102.45=0.38%） |
| LLM 等待占比 | 公式 = (墙钟 − CPU 总时间) ÷ 墙钟 × 100% | 同上（macOS 99.52%；lab 99.90%；orangepi 99.37%；Windows >99.6%）。**严谨性说明**：严格讲这是「非计算时间占比」——本负载下非计算时间全部用于等待外部 LLM 响应/网络，故即 LLM/网络等待占比 |
| 峰值 RSS | 进程最大常驻内存，统一为 MiB | macOS / Linux x86_64：`/usr/bin/time` 的 `ru_maxrss`（macOS 字节 ÷ 1,048,576 = 55.2；Linux KB ÷ 1024 = 44.2/44.1）；Linux aarch64：python3 `RUSAGE_CHILDREN` ru_maxrss（42.5）；Windows：`Get-Process WorkingSet` 250 ms 轮询取 max（46.2 MiB，396 个采样点） |
| 空载 RSS | serve 启动就绪后固定间隔采样取稳态值 | macOS / Linux：8 次 × 1 s `ps -o rss`（macOS 20,976 KB 稳定 = 20.5 MiB；lab 11,796 KB = 11.5；orangepi 8,836 KB = 8.6）；Windows：8 × 1 s 轮询（serve_rss_mib=12.594 ×8 稳定） |
| 冷启动 `--version` | 多次采样取 min / median | macOS / Linux x86_64：8 次计时（macOS min 8.3 / median 8.5；lab min 6 / median 6.5）；Linux aarch64：8 次（min 8 / median 10）；Windows：`Measure-Command` 8 次（min 36.2 / median 37.6；峰值 RSS 1 ms 轮询近似 10.2–10.7 MiB） |
| 镜像体积 | 内容大小与磁盘占用两种口径 | `docker image inspect .Size`（macOS 62,849,550 B = 62.9 MB content；lab 167,640,501 B ≈ 168 MB；orangepi 57,640,353 B）与 `docker images` DISK USAGE（macOS 282 MB / lab 168 MB / orangepi 231 MB）——content 与 disk 口径不可直接换算，并列展示；Windows 未装 Docker，N/A |
| 评审对象参数量 | 文件数 / 行数 / diff 增删 / 估算 token | perf-bench-repo 生成器冻结规格：20 文件 / 2000 行 / 10 文件 +150/−40；三平台 diff 文本 11,606 B + 改动内容 24,828 B → 估算 ≈10.4k token（÷3.5，不含模板）；Windows 侧 diff 19,978 B → diff ≈5.0k token、全流程（含系统提示与文件清单）≈15–25k（量级标注，未按具体 tokenizer 实测） |

#### D. 客观性清单（统一基线下的残余差异与边界）

- **统一了版本 / 模型 / 评审对象**：四平台均 v0.9.14、deepseek-v4-flash、perf-bench-repo（tree hash 三机一致），墙钟 / CPU 具备横向可比基础；
- **直连 vs 代理**：同一对象下 lab 直连 107.61 vs 代理 114.86（快 6.3%）、orangepi 直连 97.07 vs 代理 112.33（快 13.6%）——代理仅带来约 6–14% 延迟；而 macOS 直连 50.02 s vs Linux 直连 97–108 s（约 2×）的主因是**网络 RTT**（macOS 到 api.deepseek.com 单次连通 0.085–0.099 s，Linux 直连 1.47–1.61 s，约 15×），叠加每专家多次往返放大；三平台 CPU 均 <0.7 s，墙钟与本地计算无关；
- **残余真实差异（不掩盖）**：① aarch64 CPU 总时间 0.615–0.625 s 是 x86（0.11–0.12 s）的 5–6×、macOS（0.24 s）的 2.6×——RK3588 慢核真实平台差，多次运行稳定；比值指标（平均 <1%、等待 >99%）不受影响；② macOS 峰值 RSS 55.2 MiB 高于 Linux 42–44 MiB——统一对象后确认为真实平台差（macOS 分配器/运行时）；③ Windows 冷启动 36–38 ms 为四平台最慢（PE 加载 + Defender 实时扫描 + CRT 初始化），每次调用仅发生一次，无优化价值；
- **样本数与稳定性**：每平台 1–2 次真实付费评审；CPU 时间跨多次运行稳定（x86 恒 0.11–0.12 s、aarch64 恒 0.615–0.625 s、macOS 0.24 s）；峰值% 采样（5.7 / 7.5 / 17.0 / 24.18%）与客观平均（0.10–0.63%）无对应，系采样噪声，不构成资源差异证据；
- **旧对象 / 旧版本数据不可横向对比**（单独标注，不参与四平台结论）：
  - **2026-08-03 基线（v0.9.x 早期）**：x86 空载 ~36 / 峰值 ~39 MiB、aarch64 空载 ~28 / 峰值 ~31 MiB、墙钟 14–18.5 s（直连、小 diff）——版本与对象均不同；
  - **2026-08-12 早期小 diff 对象（非 perf-bench-repo）**：macOS gitlab-ee.md 6+/6- 21.24 s、lab cf75712 14+/35- 98.24–109.36 s、orangepi 同对象 86.04–107.43 s——对象不同，仅存档。

### 部署时长

- **零编译镜像**（v0.9.9+）：`Dockerfile` 不再编译 Rust / Vue，镜像构建 = 纯下载 release 资产（二进制 + 前端 dist，sha256 校验），**数分钟**完成，时长由网络带宽决定；
- **GHCR 镜像**（v0.9.10+）：`docker pull ghcr.io/liewzheng/review-engine:latest`，无需 clone 仓库，秒级到分钟级。

### 稳定性基线

- `cargo fmt` / `clippy` **零警告**；
- **900+ 项自动化测试**持续守护；
- 每轮发版前，用项目自己的 AI 专家阵容审查本仓库自身的 diff（见下文「安全」）。

## 部署形态

- **install.sh 一键安装**：下载单个静态二进制，自动校验 sha256 并落位 `~/.local/bin`，同时创建 `reng` 符号链接；
- **Homebrew tap**：`brew install review-engine`，随 Homebrew 生态管理升级；
- **Docker 镜像**：内置 `Dockerfile` 与 `docker-compose.yml`，一条 `docker compose up -d` 拉起 Server 形态，跨架构（x86-64 / aarch64）镜像开箱即用；
- **CLI / Server 双形态**：本地开发者用 CLI 做日常评审与审计；团队用 Server 集中管理配置、评审历史与升级。

## 安全

- **自升级链路可信**：下载资产经 sha256 校验、解压防路径逃逸与解压炸弹、替换前备份 + 冒烟检查失败自动回滚——把「升级」这件事本身也当供应链攻击面来防；
- **API 默认安全**：恒定时间比较、非回环绑定强制要求 token、无 token 一律 401；
- **发布流程内置自我审查**：每轮合并前，用本项目自己的多专家 AI 阵容审查本仓库自身的 diff。这套机制在历史上实证抓出并修复过鉴权绕过、生产构建白屏等真实缺陷——我们用自己写的工具，审查自己写的代码。

## 结语

一个几十 MiB 的二进制，装下了一支 11 人 AI 评审团队；一轮多专家评审本机只需零点几秒 CPU（墙钟主要花在等 AI 回复上），四平台资源占用低到可以忽略；升级、鉴权、部署、跨架构，处处顺手且默认安全。

review-engine 想做的事很简单：**让高质量的代码评审，成为每一次提交的默认动作，而不是团队的奢望。**
