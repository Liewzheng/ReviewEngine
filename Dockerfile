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
RUN groupadd -r review-engine && useradd -r -g review-engine -d /app -s /sbin/nologin review-engine

WORKDIR /app

# ── 零编译构建参数 ────────────────────────────────────────────────────────
# REVIEW_ENGINE_VERSION:必填,对应 GitHub Release 的 tag(如 v0.9.8),决定下载
#   哪个版本的二进制与前端 dist。留空则构建立即失败(fail-fast),防静默用错版本。
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
RUN test -n "$REVIEW_ENGINE_VERSION" \
      || { echo "ERROR: REVIEW_ENGINE_VERSION build-arg is required (e.g. v0.9.8)"; exit 1; } \
    && case "$(uname -m)" in \
         x86_64|amd64)  TRIPLE="x86_64-unknown-linux-gnu" ;; \
         aarch64|arm64) TRIPLE="aarch64-unknown-linux-gnu" ;; \
         *) echo "ERROR: unsupported architecture: $(uname -m)"; exit 1 ;; \
       esac \
    && echo ">> [1/3] download review-engine-${TRIPLE}.tar.gz (${REVIEW_ENGINE_VERSION})" \
    && curl -fsSL --retry 3 --connect-timeout 15 \
         -o "review-engine-${TRIPLE}.tar.gz" \
         "${REVIEW_ENGINE_BASE_URL}/${REVIEW_ENGINE_VERSION}/review-engine-${TRIPLE}.tar.gz" \
    && echo ">> [2/3] verify sha256 (${REVIEW_ENGINE_VERSION})" \
    && curl -fsSL --retry 3 --connect-timeout 15 \
         -o "review-engine-${TRIPLE}.sha256" \
         "${REVIEW_ENGINE_BASE_URL}/${REVIEW_ENGINE_VERSION}/review-engine-${TRIPLE}.sha256" \
    && sha256sum -c "review-engine-${TRIPLE}.sha256" \
    && tar -xzf "review-engine-${TRIPLE}.tar.gz" -C /usr/local/bin \
    && rm -f "review-engine-${TRIPLE}.tar.gz" "review-engine-${TRIPLE}.sha256" \
    && /usr/local/bin/review-engine --version

# 下载前端 dist 并解包到 /app/frontend/dist
# frontend-dist.tar.gz 由 release.yml 的 upload-frontend-dist job 打包(-C dist .),
# 包根即 index.html + assets/,直接 -C 解包即可。该资产暂未发布 .sha256 副件,
# 故前端校验可选:副件存在则校验,404 则告警跳过(与 install.sh 校验策略一致)。
RUN test -n "$REVIEW_ENGINE_VERSION" \
      || { echo "ERROR: REVIEW_ENGINE_VERSION build-arg is required (e.g. v0.9.8)"; exit 1; } \
    && echo ">> [3/3] download frontend-dist.tar.gz (${REVIEW_ENGINE_VERSION})" \
    && mkdir -p /app/frontend/dist \
    && curl -fsSL --retry 3 --connect-timeout 15 \
         -o frontend-dist.tar.gz \
         "${REVIEW_ENGINE_BASE_URL}/${REVIEW_ENGINE_VERSION}/frontend-dist.tar.gz" \
    && if curl -fsSL --retry 3 --connect-timeout 15 \
         -o frontend-dist.tar.gz.sha256 \
         "${REVIEW_ENGINE_BASE_URL}/${REVIEW_ENGINE_VERSION}/frontend-dist.tar.gz.sha256"; then \
         sha256sum -c frontend-dist.tar.gz.sha256 || exit 1; \
       else \
         echo "WARN: frontend-dist.tar.gz.sha256 不存在,跳过前端资产校验"; \
       fi \
    && tar -xzf frontend-dist.tar.gz -C /app/frontend/dist \
    && rm -f frontend-dist.tar.gz frontend-dist.tar.gz.sha256 \
    && ls -la /app/frontend/dist

# reng 别名（argv[0] 动态命令名，symlink 调用即可生效）
RUN ln -s /usr/local/bin/review-engine /usr/local/bin/reng

# 复制启动入口脚本,保留作为容器入口点以便后续扩展
COPY entrypoint.sh /app/entrypoint.sh
RUN chmod +x /app/entrypoint.sh

# 创建配置和报告目录
RUN mkdir -p /app/config /app/reports /app/.ssh && \
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
