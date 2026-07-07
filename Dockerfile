# Multi-stage build for review-engine SaaS deployment
# Targets: GitLab EE self-hosted integration
# Uses China mainland mirrors for faster builds

# ═══════════════════════════════════════════════════════════════════════
# Stage 1: Rust Builder
# ═══════════════════════════════════════════════════════════════════════
FROM ubuntu:22.04 AS builder

WORKDIR /build

# 配置 apt 国内镜像源（阿里云）
RUN sed -i 's|archive.ubuntu.com|mirrors.aliyun.com|g' /etc/apt/sources.list \
    && sed -i 's|security.ubuntu.com|mirrors.aliyun.com|g' /etc/apt/sources.list

# 安装构建依赖
RUN apt-get update && apt-get install -y --no-install-recommends \
    curl \
    ca-certificates \
    build-essential \
    pkg-config \
    libssl-dev \
    git \
    && rm -rf /var/lib/apt/lists/*

# 安装 Rust（使用中国科技大学镜像）
ENV RUSTUP_DIST_SERVER=https://mirrors.ustc.edu.cn/rust-static
ENV RUSTUP_UPDATE_ROOT=https://mirrors.ustc.edu.cn/rust-static/rustup
RUN curl --proto '=https' --tlsv1.2 -sSf https://mirrors.ustc.edu.cn/rust-static/rustup/rustup-init.sh | sh -s -- -y --default-toolchain stable
ENV PATH="/root/.cargo/bin:${PATH}"

# 配置 Cargo 国内镜像源（中国科技大学 sparse 索引）
RUN mkdir -p /root/.cargo \
    && echo '[registries]' > /root/.cargo/config.toml \
    && echo 'crates-io = { index = "sparse+https://mirrors.ustc.edu.cn/crates.io-index/" }' >> /root/.cargo/config.toml

# 复制依赖清单（用于缓存层）
COPY Cargo.toml Cargo.lock ./

# 复制源代码
COPY src ./src
COPY docs ./docs

# 构建 release 二进制（不含 python 特性以最小化依赖）
RUN cargo build --release --no-default-features --features cli

# ═══════════════════════════════════════════════════════════════════════
# Stage 1.5: Frontend Builder (Node.js)
# ═══════════════════════════════════════════════════════════════════════
FROM ubuntu:22.04 AS frontend-builder

WORKDIR /frontend

# 配置 apt 国内镜像源
RUN sed -i 's|archive.ubuntu.com|mirrors.aliyun.com|g' /etc/apt/sources.list \
    && sed -i 's|security.ubuntu.com|mirrors.aliyun.com|g' /etc/apt/sources.list

# 安装 Node.js（使用阿里云镜像）
RUN apt-get update && apt-get install -y --no-install-recommends \
    curl \
    ca-certificates \
    && curl -fsSL https://mirrors.aliyun.com/nvm/gpg.key | gpg --dearmor -o /usr/share/keyrings/nvm.gpg \
    && echo 'deb [signed-by=/usr/share/keyrings/nvm.gpg] https://mirrors.aliyun.com/nvm/ stable main' | tee /etc/apt/sources.list.d/nvm.list \
    && apt-get update && apt-get install -y nvm \
    && . /usr/share/nvm/init-nvm.sh \
    && nvm install 20 \
    && nvm use 20 \
    && rm -rf /var/lib/apt/lists/*

ENV PATH="/root/.nvm/versions/node/v20.18.0/bin:${PATH}"

# 配置 npm 国内镜像
RUN npm config set registry https://registry.npmmirror.com

# 复制前端代码
COPY frontend/package.json frontend/package-lock.json ./
RUN npm install --prefer-offline --no-audit --no-fund

COPY frontend ./
RUN npm run build

# ═══════════════════════════════════════════════════════════════════════
# Stage 2: Runtime
# ═══════════════════════════════════════════════════════════════════════
FROM ubuntu:22.04 AS runtime

# 配置 apt 国内镜像源（阿里云）
RUN sed -i 's|archive.ubuntu.com|mirrors.aliyun.com|g' /etc/apt/sources.list \
    && sed -i 's|security.ubuntu.com|mirrors.aliyun.com|g' /etc/apt/sources.list

# 安装运行时依赖
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    git \
    openssh-client \
    curl \
    && rm -rf /var/lib/apt/lists/* \
    && apt-get clean

# 创建非 root 用户
RUN groupadd -r review-engine && useradd -r -g review-engine -d /app -s /sbin/nologin review-engine

WORKDIR /app

# 从 builder 复制二进制
COPY --from=builder /build/target/release/review-engine /usr/local/bin/review-engine

# 复制前端构建产物
COPY --from=frontend-builder /frontend/dist /app/frontend/dist

# 创建配置和报告目录
RUN mkdir -p /app/config /app/reports /app/.ssh && \
    chown -R review-engine:review-engine /app

# 切换到非 root 用户
USER review-engine

# 暴露默认服务端口
EXPOSE 8080

# 健康检查
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD curl -f http://localhost:8080/health || exit 1

# 默认环境变量
ENV REVIEW_ENGINE_CONFIG_DIR=/app/config
ENV REVIEW_ENGINE_REPORT_DIR=/app/reports
ENV RUST_LOG=info

# 入口：启动服务
ENTRYPOINT ["/usr/local/bin/review-engine"]
CMD ["serve", "--bind", "0.0.0.0", "--port", "8080"]
