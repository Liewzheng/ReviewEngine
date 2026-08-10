#!/bin/bash
set -e

# ── Native TLS (serve --tls-cert/--tls-key/--tls-port) ──────────────────
# HTTPS 仅在 REVIEW_TLS_CERT 与 REVIEW_TLS_KEY 均非空时启用(它们是开关,
# 不是文件路径)。证书/私钥由 docker-compose 以只读卷挂载到固定容器路径:
#   /app/tls/cert.pem   /app/tls/key.pem
# 两者任一为空 → 不追加任何 TLS 参数,serve 保持纯 HTTP 8080(fail-soft),
# 无证书也能正常启动,行为与旧版本一致。
if [ -n "${REVIEW_TLS_CERT}" ] && [ -n "${REVIEW_TLS_KEY}" ]; then
    set -- "$@" --tls-cert /app/tls/cert.pem --tls-key /app/tls/key.pem --tls-port 8443
fi

exec /usr/local/bin/review-engine "$@"
