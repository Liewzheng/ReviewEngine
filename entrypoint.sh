#!/bin/bash
set -e

# ── First-boot sync:把镜像内容同步进可写部署卷 ────────────────────────────
# 容器以 USER review-engine(非 root)运行;/usr/local/bin/review-engine 与
# /app/frontend-dist-image 是镜像只读层。compose 把 ./bin、./frontend-dist 两个
# 可写卷分别挂到 /app/bin、/app/frontend/dist,供容器内自动升级(T27)写入——
# 升级即替换卷里的二进制 / dist,完成后进程 exit(0) 触发 restart: unless-stopped
# 拉起新版本。首次启动卷为空,必须把镜像内容同步进卷,之后一律从卷运行:
#   - /app/bin/review-engine 不存在 → 从 /usr/local/bin/review-engine 复制;
#   - /app/frontend/dist 无 index.html → 从 /app/frontend-dist-image 复制。
# 用 `[ ! -f ... ]` 而非 ls 判空:卷已有内容(如已被升级写入新版本)绝不覆盖。
# 注意:Linux NAS 上 bind 卷继承宿主属主,若容器内写卷失败,需先对宿主目录
# chown 到容器 UID(见 docker-compose.yml 注释),否则首次同步会报错。
if [ ! -f /app/bin/review-engine ]; then
    echo ">> first-boot: 同步二进制 /usr/local/bin/review-engine -> /app/bin/review-engine"
    mkdir -p /app/bin
    cp /usr/local/bin/review-engine /app/bin/review-engine
    chmod +x /app/bin/review-engine
else
    echo ">> /app/bin/review-engine 已存在,跳过二进制同步(保留卷内当前版本)"
fi

if [ ! -f /app/frontend/dist/index.html ]; then
    if [ -f /app/frontend-dist-image/index.html ]; then
        echo ">> first-boot: 同步前端 dist /app/frontend-dist-image -> /app/frontend/dist"
        mkdir -p /app/frontend/dist
        cp -a /app/frontend-dist-image/. /app/frontend/dist/
    else
        echo "WARN: 镜像内无 /app/frontend-dist-image(该 release 未含前端资产),跳过前端同步;serve 用 coming soon 占位页"
    fi
else
    echo ">> /app/frontend/dist/index.html 已存在,跳过前端同步(保留卷内当前版本)"
fi

# ── Native TLS (serve --tls-cert/--tls-key/--tls-port) ──────────────────
# HTTPS 仅在 REVIEW_TLS_CERT 与 REVIEW_TLS_KEY 均非空时启用(它们是开关,
# 不是文件路径)。证书/私钥由 docker-compose 以只读卷挂载到固定容器路径:
#   /app/tls/cert.pem   /app/tls/key.pem
# 两者任一为空 → 不追加任何 TLS 参数,serve 保持纯 HTTP 8080(fail-soft),
# 无证书也能正常启动,行为与旧版本一致。
if [ -n "${REVIEW_TLS_CERT}" ] && [ -n "${REVIEW_TLS_KEY}" ]; then
    set -- "$@" --tls-cert /app/tls/cert.pem --tls-key /app/tls/key.pem --tls-port 8443
fi

# 从卷运行:升级替换的就是 /app/bin/review-engine,exec 后进程即新版本
exec /app/bin/review-engine "$@"
