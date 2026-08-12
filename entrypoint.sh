#!/bin/bash
set -e

# ── 可测试路径(默认即生产路径;冒烟测试通过环境变量重定向到临时目录)──────────
IMAGE_BIN="${REVIEW_IMAGE_BIN:-/usr/local/bin/review-engine}"
VOL_BIN="${REVIEW_VOL_BIN:-/app/bin/review-engine}"
IMAGE_DIST="${REVIEW_IMAGE_DIST:-/app/frontend-dist-image}"
VOL_DIST="${REVIEW_VOL_DIST:-/app/frontend/dist}"

# ── 版本工具 ────────────────────────────────────────────────────────────────
# 解析 `<bin> --version` 输出(形如 "Review Engine v0.9.14")为 "vX.Y.Z";
# 解析失败(非预期输出,或 --version 退出码非 0)返回非 0,调用方按 fail-safe 处理。
ver_parse() {
    local out
    out="$("$1" --version 2>/dev/null)" || return 1
    if [[ "$out" =~ v[0-9]+\.[0-9]+\.[0-9]+ ]]; then
        printf '%s' "${BASH_REMATCH[0]}"
        return 0
    fi
    return 1
}

# 比较两个 "vX.Y.Z"(major.minor.patch 数值比较):$1 > $2 时返回 0,否则返回 1。
ver_gt() {
    local re='^v([0-9]+)\.([0-9]+)\.([0-9]+)$'
    [[ "$1" =~ $re ]] || return 1
    local -i a1="${BASH_REMATCH[1]}" a2="${BASH_REMATCH[2]}" a3="${BASH_REMATCH[3]}"
    [[ "$2" =~ $re ]] || return 1
    local -i b1="${BASH_REMATCH[1]}" b2="${BASH_REMATCH[2]}" b3="${BASH_REMATCH[3]}"
    if (( a1 > b1 )); then return 0; fi
    if (( a1 < b1 )); then return 1; fi
    if (( a2 > b2 )); then return 0; fi
    if (( a2 < b2 )); then return 1; fi
    if (( a3 > b3 )); then return 0; fi
    return 1
}

# ── 版本感知同步:镜像升级时自动覆盖卷内旧二进制 / dist ──────────────────────
# 背景:./bin 与 ./frontend-dist 是 compose 挂载的可写 bind 卷(供容器内自动
# 升级 UI Upgrade / POST /api/v1/system/upgrade 写入)。老逻辑只在卷为空时同步
# 镜像内容,导致 `docker pull` 新镜像后卷内仍跑旧二进制(用户 NAS 实测:镜像
# v0.9.14、卷内 v0.9.12,WebUI 显示旧版本、升级报 no asset)。
# 新逻辑:启动时比较镜像二进制与卷二进制的版本——
#   - 卷无二进制   → 无条件从镜像同步(首次启动,原行为);
#   - 镜像版本 > 卷版本 → 镜像覆盖卷(cp + chmod),前端 dist 联动同步;
#   - 卷版本 >= 镜像版本 → 保留卷(兼容容器内自升级:新版本已写进卷,不降级)。
# fail-safe:任一 --version 解析失败 → 不覆盖,保留卷内现状,打印 WARN 并继续
# 启动——绝不因同步逻辑导致容器起不来。同步段内部失败(cp/mkdir 等)同样用
# if/|| 包裹,只 WARN 不中断启动。TLS 与 exec 逻辑不受影响。
sync_bin=0
if [ ! -f "$VOL_BIN" ]; then
    echo ">> sync: 卷内无二进制,从镜像同步(首次启动)"
    sync_bin=1
else
    img_ver=""
    vol_ver=""
    if img_ver="$(ver_parse "$IMAGE_BIN")" && vol_ver="$(ver_parse "$VOL_BIN")"; then
        if ver_gt "$img_ver" "$vol_ver"; then
            echo ">> sync: 镜像 $img_ver 覆盖卷内 $vol_ver"
            sync_bin=1
        else
            echo ">> sync: 卷内 $vol_ver 已是最新/更新,保留卷内二进制(不降级);dist 一并保留"
        fi
    else
        echo "WARN: 二进制版本解析失败,跳过自动同步,保留卷内现状继续启动"
    fi
fi

if [ "$sync_bin" = 1 ]; then
    echo ">> sync: 复制镜像二进制 $IMAGE_BIN -> $VOL_BIN"
    mkdir -p "$(dirname "$VOL_BIN")" || echo "WARN: mkdir $(dirname "$VOL_BIN") 失败,继续启动"
    if cp "$IMAGE_BIN" "$VOL_BIN"; then
        chmod +x "$VOL_BIN" || echo "WARN: chmod $VOL_BIN 失败,继续启动"
    else
        echo "WARN: 复制二进制失败,保留卷内现状,继续启动"
    fi
    # 前端 dist 与二进制联动:仅当本次二进制从镜像同步到卷时才同步 dist
    if [ -f "$IMAGE_DIST/index.html" ]; then
        echo ">> sync: 复制前端 dist $IMAGE_DIST -> $VOL_DIST(与二进制联动)"
        mkdir -p "$VOL_DIST" || echo "WARN: mkdir $VOL_DIST 失败,继续启动"
        cp -a "$IMAGE_DIST/." "$VOL_DIST/" || echo "WARN: 复制 dist 失败,继续启动"
    else
        echo "WARN: 镜像内无 $IMAGE_DIST/index.html(该 release 未含前端资产),跳过前端同步"
    fi
else
    echo ">> sync: 卷二进制被保留(卷>=镜像),dist 保留卷内现状"
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

# 从卷运行:升级替换的就是 $VOL_BIN,exec 后进程即新版本
exec "$VOL_BIN" "$@"
