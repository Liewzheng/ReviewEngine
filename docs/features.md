# review-engine 特性概览

> 本文为宣传向特性文档，以中文撰写；如需要，可提供英文版（English version available on request）。
> 文中性能与资源数据以 **v0.9.14 四平台实测（2026-08-12：macOS arm64 / Linux x86_64 / Linux aarch64）** 为主；2026-08-03 旧版本基线（v0.9.x 早期）单独标注、不参与对比。只陈述事实，不夸大。

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

> **版本基线：下表全部为 v0.9.14 实测（2026-08-12）。** macOS 为 cargo release 构建 v0.9.14；Linux x86_64 为官方 release 资产 v0.9.14（sha256 校验）；Linux aarch64 因官方资产依赖 GLIBC_2.39、目标机为 glibc 2.35，改用同 tag 源码 `cargo build --release`（v0.9.14）。各平台评审均为真实 LLM 调用（11 位 AI 专家）——**评审是 LLM 调用型负载，墙钟受网络/模型响应影响，本地负载以 CPU 时间为准**。
>
> 测试环境：
> - **macOS arm64**：Apple Silicon，macOS 26.6.1，128 GiB 内存，Docker 29.5.3；
> - **Linux x86_64**（lab-mixosense）：13th Gen Intel Core i5-1340P（12C/16T），14 GiB 内存（可用 ~8.9 GiB），Ubuntu（glibc 2.43），Docker 28.4.0；
> - **Linux aarch64**（orangepi5）：RK3588（4×Cortex-A76 + 4×Cortex-A55），7.7 GiB 内存（可用 ~5.8 GiB），Ubuntu 22.04（glibc 2.35），Docker 29.5.3；
> - **Windows**：待补。

### 资源占用（v0.9.14 四平台）

四平台结论一致：**评审是典型的 LLM I/O 等待型负载**——绝大多数时间在等模型输出，CPU 几乎不忙。

| 指标 | macOS arm64 | Linux x86_64 | Linux aarch64 | Windows |
|---|---|---|---|---|
| 空载常驻内存（RSS） | 20.5 MiB | 11.5 MiB | 8.6 MiB | 待补 |
| 评审峰值内存（RSS） | 55.2 MiB | 45.1 MiB | 43.1 MiB | 待补 |
| 评审 CPU 峰值 | 5.0%（单核） | 7.5% | 17.0% | 待补 |
| 评审平均 CPU | 0.75% | 0.11% | <1%（推断） | 待补 |
| 一次完整评审（墙钟） | 21.2–21.7 s | 98.2 s | ≈83 s | 待补 |
| 冷启动 `--version` | 8.3 ms | 6 ms | 8 ms | 待补 |
| 镜像体积 | 282 MB disk / 62.9 MB content | 168 MB（disk） | 231 MB（disk） | 待补 |
| 容器空载内存 | 3.5 MiB | 3.6–4.4 MiB | 1.98–2.38 MiB | 待补 |

口径说明：

- **内存**统一为 MiB（1 MiB = 1,048,576 B）；评审峰值 RSS 来自内核记账（ru_maxrss），与逐秒采样吻合。
- **镜像体积**口径不同：macOS 为 Docker Desktop 的 DISK USAGE / CONTENT SIZE（282 MB / 62.9 MB）；Linux 两机为 `docker images` 的 DISK USAGE（168 / 231 MB）——只列不换算。macOS / Linux 本地镜像内嵌二进制分别为 v0.9.12 / v0.9.6（落后源码一至两版），体积数据来自这些镜像；CPU / 内存数据来自 v0.9.14 二进制。
- **评审时长**：Linux 两机经代理（192.168.1.180:7890）访问 LLM API，墙钟 83–98 s 主要受代理中转延迟影响；macOS 直连为 21.2–21.7 s。评审为 LLM 调用型负载，墙钟受网络/模型响应影响——本地 CPU 实测仅 0.11–0.16 s（macOS 0.16 s、Linux x86_64 0.11 s；Linux aarch64 因方法缺口未单独捕获，推断 <1%，与另两平台同负载特征），墙钟差异与本地资源无关。
- **CPU 峰值口径**：macOS / Linux x86_64 为逐秒采样峰值；Linux aarch64 为 ps 生命周期平均口径（该机相对慢，启动/解析占比放大），仅作量级参考。

资源余量：四平台容器空载内存 2–4.4 MiB、CPU 稳态 0.00%，评审期间本地 CPU 平均 <1%——把它塞进小规格主机，也只占一个角落，把资源留给真正在跑的模型。

### 旧版本基线（2026-08-03，v0.9.x 早期——仅对照，不与上方 v0.9.14 混比）

2026-08-03 双机实测（当时 **v0.9.x 早期**版本，非 v0.9.14）：

| 指标 | x86-64 主机 | aarch64 主机 |
|---|---|---|
| 空载常驻内存（RSS） | ~36 MiB | ~28 MiB |
| 评审峰值内存（RSS） | ~39 MiB | ~31 MiB |
| 评审期间 CPU 峰值 | 0.26% | 5.23% |
| 镜像体积 | 168 MB | 231 MB |
| 一次完整评审（墙钟，直连） | 14–18.5 s | 14–18.5 s |

环境：x86-64 主机（多核 CPU / 约 14 GiB 内存 / NVMe SSD）、aarch64 主机（8 核 CPU / 约 8 GiB 内存 / NVMe），评审直连未走代理；容器限额下 x86-64 余量约 98%、aarch64 为实际用量的 64 倍。该基线版本早于 v0.9.14，仅作演进对照，不参与 v0.9.14 结论。

### 评审性能

一次真实的完整评审（**11 位 AI 专家**协同），v0.9.14 四平台实测墙钟 **21–98 s**（macOS 直连 21.2–21.7 s；Linux 经代理 83–98 s），但本地 CPU 平均均 **<1%**——墙钟差异来自外部 LLM 与网络路径，不是本机负载。直连场景下十余秒到二十余秒一轮多专家评审，性价比不言自明。

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

一个几十 MiB 的二进制，装下了一支 11 人 AI 评审团队；十几秒完成一轮多专家评审，双机资源占用低到可以忽略；升级、鉴权、部署、跨架构，处处顺手且默认安全。

review-engine 想做的事很简单：**让高质量的代码评审，成为每一次提交的默认动作，而不是团队的奢望。**
