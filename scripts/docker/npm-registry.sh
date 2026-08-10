#!/bin/sh
# npm registry 主备自动选择:IP 地区探测 + NPM_REGISTRY 显式覆盖
#
# 用途:Dockerfile frontend 阶段(node:22-alpine,busybox sh,有 wget 无 curl)。
# 策略与 builder 阶段 apt/cargo 国内镜像一致:CN 网络 → npmmirror 主;
# 非 CN → 官方主;探测失败按非 CN 处理(官方主,镜像备,保证可用)。
#
# 覆盖:环境变量 NPM_REGISTRY 非空时跳过 IP 探测,主=指定源,备=官方源。
# 输出两行:
#   PRIMARY=<url>
#   FALLBACK=<url>
set -eu

OFFICIAL="https://registry.npmjs.org/"
MIRROR="https://registry.npmmirror.com"

# ── 1) 显式指定:跳过 IP 检测 ──────────────────────────────────────────
if [ -n "${NPM_REGISTRY:-}" ]; then
    echo "PRIMARY=${NPM_REGISTRY}"
    echo "FALLBACK=${OFFICIAL}"
    exit 0
fi

# ── 2) 探测出口 IP 地区(两个探测源,都失败则视为非 CN)─────────────────
country=""
if command -v wget >/dev/null 2>&1; then
    country=$(wget -qO- --timeout=3 "http://ip-api.com/line/?fields=countryCode" 2>/dev/null || true)
fi
if [ -z "$country" ] && command -v curl >/dev/null 2>&1; then
    country=$(curl -s --max-time 3 "http://ip-api.com/line/?fields=countryCode" 2>/dev/null || true)
fi
if [ -z "$country" ]; then
    if command -v wget >/dev/null 2>&1; then
        country=$(wget -qO- --timeout=3 "https://ipinfo.io/country" 2>/dev/null || true)
    fi
    if [ -z "$country" ] && command -v curl >/dev/null 2>&1; then
        country=$(curl -s --max-time 3 "https://ipinfo.io/country" 2>/dev/null || true)
    fi
fi

# ── 3) 地区判定 ────────────────────────────────────────────────────────
case "${country}" in
    CN)
        echo "PRIMARY=${MIRROR}"
        echo "FALLBACK=${OFFICIAL}"
        ;;
    *)
        echo "PRIMARY=${OFFICIAL}"
        echo "FALLBACK=${MIRROR}"
        ;;
esac
exit 0
