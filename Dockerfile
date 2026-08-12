# Zero-build image for review-engine SaaS deployment
# Targets: GitLab EE self-hosted integration
# 零编译:镜像构建 = 纯下载。运行时二进制与前端 dist 直接下载自 GitHub Release
# 资产,容器内不再编译 Rust / Vue——NAS 上 10-30 分钟的编译与易挂的 npm 网络
# 在此消除(原 builder + frontend 两编译阶段已整体移除)。

# ═══════════════════════════════════════════════════════════════════════
# Stage 1: Runtime(零编译)
# ═══════════════════════════════════════════════════════════════════════
# 基础镜像用 24.04(glibc 2.39)而非 22.04(glibc 2.35):release 二进制在 GitHub
# Actions ubuntu-latest(= 24.04)上构建,实测需要 GLIBC_2.39,22.04 起不来。
FROM ubuntu:24.04

# 配置 apt 国内镜像源(阿里云):x86_64 走 archive.ubuntu.com,aarch64 走
# ports.ubuntu.com(ubuntu-ports)——两处都替换,否则 ARM 镜像构建会卡在官方源。
RUN sed -i 's|archive.ubuntu.com|mirrors.aliyun.com|g' /etc/apt/sources.list \
    && sed -i 's|security.ubuntu.com|mirrors.aliyun.com|g' /etc/apt/sources.list \
    && sed -i 's|ports.ubuntu.com|mirrors.aliyun.com|g' /etc/apt/sources.list

# 安装运行时依赖(tar 用于解包下载的 release 资产)
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    git \
    openssh-client \
    curl \
    tar \
    && rm -rf /var/lib/apt/lists/* \
    && apt-get clean

# 创建非 root 用户
# 固定 UID/GID(9001):useradd -r 的 UID 由构建系统分配,不可靠(NAS 上实测撞车)。
# 9001 避开宿主常见 UID(1000)与被占用的 999;改此值须同步文档中的 chown 引导。
RUN groupadd -r -g 9001 review-engine && useradd -r -u 9001 -g review-engine -d /app -s /sbin/nologin review-engine

WORKDIR /app

# ── 零编译构建参数 ────────────────────────────────────────────────────────
# REVIEW_ENGINE_VERSION:可空。指定时对应 GitHub Release 的 tag(如 v0.9.8),
#   决定下载哪个版本的二进制与前端 dist;留空时自动解析 GitHub latest release
#   tag 下载(解析失败才 fail)。CI 传具体 tag 构建对应版本;本地不传即构建最新版。
# REVIEW_ENGINE_BASE_URL:Release 资产下载根地址(含 /download)。默认官方 GitHub;
#   国内网络不稳时可指向 gh-proxy.com 等镜像或内网资产服务器,便于 NAS 部署与
#   本地验证(不改默认值即官方发布路径)。
ARG REVIEW_ENGINE_VERSION=""
ARG REVIEW_ENGINE_BASE_URL="https://github.com/Liewzheng/ReviewEngine/releases/download"

# 下载并校验运行时二进制(零编译核心步骤,独立成层便于排障)
# 架构 triple 与 release 资产命名一致(x86_64 / aarch64),其余架构 fail-fast。
# 校验纪律对齐 src/upgrade:必须下载 .sha256 副件并 sha256sum -c 校验(sidecar
# 内记录完整资产文件名,故下载到 /app 时保持原名)。sidecar 命名注意:release
# 实际资产是 <triple>.sha256,不是 <archive>.tar.gz.sha256。
RUN VERSION="${REVIEW_ENGINE_VERSION:-}" \
    && if [ -z "$VERSION" ]; then \
         echo ">> REVIEW_ENGINE_VERSION 未指定,解析 GitHub latest release tag"; \
         VERSION="$(curl -fsSL --retry 2 --connect-timeout 15 -o /dev/null -w '%{url_effective}' -L "${REVIEW_ENGINE_BASE_URL%/download}/latest" | sed -n 's#.*/releases/tag/##p')"; \
         test -n "$VERSION" || { echo "ERROR: 无法解析 latest release tag(网络失败或仓库不可达)"; exit 1; }; \
         echo ">> 解析到 latest tag: ${VERSION}"; \
       fi \
    && case "$(uname -m)" in \
         x86_64|amd64)  TRIPLE="x86_64-unknown-linux-gnu" ;; \
         aarch64|arm64) TRIPLE="aarch64-unknown-linux-gnu" ;; \
         *) echo "ERROR: unsupported architecture: $(uname -m)"; exit 1 ;; \
       esac \
    && echo ">> [1/3] download review-engine-${TRIPLE}.tar.gz (${VERSION})" \
    && curl -fsSL --retry 3 --connect-timeout 15 \
         -o "review-engine-${TRIPLE}.tar.gz" \
         "${REVIEW_ENGINE_BASE_URL}/${VERSION}/review-engine-${TRIPLE}.tar.gz" \
    && echo ">> [2/3] verify sha256 (${VERSION})" \
    && curl -fsSL --retry 3 --connect-timeout 15 \
         -o "review-engine-${TRIPLE}.sha256" \
         "${REVIEW_ENGINE_BASE_URL}/${VERSION}/review-engine-${TRIPLE}.sha256" \
    && sha256sum -c "review-engine-${TRIPLE}.sha256" \
    && tar -xzf "review-engine-${TRIPLE}.tar.gz" -C /usr/local/bin \
    && rm -f "review-engine-${TRIPLE}.tar.gz" "review-engine-${TRIPLE}.sha256" \
    && /usr/local/bin/review-engine --version

# 下载前端 dist 并解包到镜像内备份位置 /app/frontend-dist/image(frontend-dist.tar.gz
# 由 release.yml 的 upload-frontend-dist job 打包,-C dist . 使包根即 index.html +
# assets/)。注意:解包目标不是 /app/frontend/dist——compose 会把 ./frontend-dist 卷
# 挂到 /app/frontend/dist(可写部署卷,遮住镜像路径),entrypoint 首次启动时从这里
# 把内容同步进卷;镜像内留副本作为同步源。
# 优雅降级:frontend-dist.tar.gz 是后续 release 才引入的资产,旧 release(如
# v0.9.8)没有它——下载 404 或 base URL 不可达时 WARN 并跳过,镜像仅含二进制、
# 仍能构建成功;/app/frontend/dist 为空时 serve 会 fallback 到 coming soon
# 占位页(可接受),待含 dist 资产的 release 重建镜像即有前端。二进制下载与
# sha256 校验保持硬失败;frontend-dist 的 sha256 副件暂未发布,其校验可选:
# 副件存在则校验,404 则告警跳过(与 install.sh 校验策略一致)。
RUN VERSION="${REVIEW_ENGINE_VERSION:-}" \
    && if [ -z "$VERSION" ]; then \
         VERSION="$(curl -fsSL --retry 2 --connect-timeout 15 -o /dev/null -w '%{url_effective}' -L "${REVIEW_ENGINE_BASE_URL%/download}/latest" | sed -n 's#.*/releases/tag/##p')"; \
         test -n "$VERSION" || { echo "ERROR: 无法解析 latest release tag(网络失败或仓库不可达)"; exit 1; }; \
       fi \
    && mkdir -p /app/frontend-dist-image \
    && echo ">> [3/3] download frontend-dist.tar.gz (${VERSION})" \
    && if curl -fsSL --retry 3 --connect-timeout 15 \
         -o frontend-dist.tar.gz \
         "${REVIEW_ENGINE_BASE_URL}/${VERSION}/frontend-dist.tar.gz"; then \
         echo "   frontend-dist.tar.gz 下载成功,校验并解包"; \
         if curl -fsSL --retry 3 --connect-timeout 15 \
              -o frontend-dist.tar.gz.sha256 \
              "${REVIEW_ENGINE_BASE_URL}/${VERSION}/frontend-dist.tar.gz.sha256"; then \
           sha256sum -c frontend-dist.tar.gz.sha256 || exit 1; \
         else \
           echo "WARN: frontend-dist.tar.gz.sha256 不存在,跳过前端资产校验"; \
         fi \
         && tar -xzf frontend-dist.tar.gz -C /app/frontend-dist-image \
         && rm -f frontend-dist.tar.gz frontend-dist.tar.gz.sha256 \
         && ls -la /app/frontend-dist-image; \
       else \
         echo "WARN: frontend-dist.tar.gz 不存在(${VERSION} release 未含该资产),跳过前端部署,镜像仅含二进制"; \
         echo "      serve 对空 /app/frontend/dist 会 fallback 到 coming soon 占位页(可接受;待含 dist 资产的 release 重建即有前端)"; \
       fi

# reng 别名（argv[0] 动态命令名，symlink 调用即可生效）
RUN ln -s /usr/local/bin/review-engine /usr/local/bin/reng

# 复制启动入口脚本,保留作为容器入口点以便后续扩展
COPY entrypoint.sh /app/entrypoint.sh
RUN chmod +x /app/entrypoint.sh

# 创建配置和报告目录
RUN mkdir -p /app/config /app/reports /app/.ssh /app/bin /app/frontend/dist && \
    chown -R review-engine:review-engine /app

# 切换到非 root 用户
USER review-engine

# 暴露端口:
#   443  = HTTPS(serve 内置 TLS,容器内 8443 → 宿主机 443)
#   8080 = 纯 HTTP(健康检查与内网直连;无证书时 fail-soft 仍走这里)
EXPOSE 443 8080

# 健康检查
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD curl -f http://localhost:8080/health || exit 1

# 默认环境变量
ENV REVIEW_ENGINE_CONFIG_DIR=/app/config
ENV REVIEW_ENGINE_REPORT_DIR=/app/reports
ENV RUST_LOG=info

# 入口：启动服务
ENTRYPOINT ["/app/entrypoint.sh"]
CMD ["serve", "--bind", "0.0.0.0", "--port", "8080"]
