#!/bin/bash
# entrypoint-sync-smoke.sh — 冒烟验证 entrypoint.sh 的版本感知同步逻辑。
#
# 不依赖 docker 构建,自包含:用 mktemp 临时目录模拟「镜像只读层」与「可写卷」,
# 通过 REVIEW_IMAGE_BIN / REVIEW_VOL_BIN / REVIEW_IMAGE_DIST / REVIEW_VOL_DIST
# 环境变量把 entrypoint.sh 的路径重定向到临时目录;截掉末尾 `exec` 行后直接
# 执行真实同步段落(仍处于 set -e 下),断言同步/保留/fail-safe 行为。
#
# 覆盖用例:
#   1  卷空             → 从镜像同步(二进制 + dist)
#   2  镜像 v0.9.14 > 卷 v0.9.12 → 覆盖二进制并联动同步 dist
#   3  卷 v0.9.14 >= 镜像 v0.9.14 → 保留卷二进制 + dist 一并保留
#   4a 镜像 --version 输出非预期   → 保留卷 + WARN
#   4b 卷 --version 退出码非 0     → 保留卷 + WARN
#   5  镜像 v0.9.10 < 卷 v0.9.14 → 不降级,保留卷
#
# 用法: bash scripts/entrypoint-sync-smoke.sh
# 全部用例通过退出 0,任一失败退出 1。

set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENTRYPOINT="$(dirname "$SCRIPT_DIR")/entrypoint.sh"

if [ ! -r "$ENTRYPOINT" ]; then
    echo "错误:找不到 $ENTRYPOINT" >&2
    exit 1
fi

PASS=0
FAIL=0
WORKS=()

# 断言辅助:$1=描述,$2=条件表达式(eval)
check() {
    local name="$1" cond="$2"
    if eval "$cond"; then
        echo "  [PASS] $name"
        PASS=$((PASS + 1))
    else
        echo "  [FAIL] $name  ->  $cond"
        FAIL=$((FAIL + 1))
    fi
}

# 制作假 review-engine 二进制:$1=路径,$2=行为
#   NONE   → 不创建(模拟卷空)
#   BAD    → --version 输出非预期(但仍退出 0)
#   EXIT1  → --version 输出看似正常但退出码非 0
#   其他   → --version 输出 "Review Engine <$2>" 并退出 0
make_bin() {
    local path="$1" behavior="$2"
    if [ "$behavior" = "NONE" ]; then
        rm -f "$path"
        return 0
    fi
    mkdir -p "$(dirname "$path")"
    {
        echo '#!/bin/bash'
        case "$behavior" in
            BAD)   echo 'echo "garbage-output"; exit 0' ;;
            EXIT1) echo 'echo "Review Engine v9.9.9"; exit 1' ;;
            *)     echo "echo 'Review Engine $behavior'; exit 0" ;;
        esac
    } > "$path"
    chmod +x "$path"
}

# 准备一次同步场景:$1=img_bin 行为,$2=vol_bin 行为,$3=img_dist 是否有 index.html(1/0)
# 设置全局:WORK / IMG_BIN_DIR / VOL_BIN_DIR / IMG_DIST / VOL_DIST / SYNC_SH / OUTPUT
prepare() {
    WORK="$(mktemp -d)"
    WORKS+=("$WORK")
    IMG_BIN_DIR="$WORK/image/bin"
    VOL_BIN_DIR="$WORK/vol/bin"
    IMG_DIST="$WORK/image/frontend-dist-image"
    VOL_DIST="$WORK/vol/frontend/dist"
    mkdir -p "$IMG_BIN_DIR" "$IMG_DIST" "$VOL_DIST"

    make_bin "$IMG_BIN_DIR/review-engine" "$1"
    make_bin "$VOL_BIN_DIR/review-engine" "$2"

    if [ "$3" = "1" ]; then
        echo '<html>NEW-DIST</html>' > "$IMG_DIST/index.html"
    fi
    # 卷 dist 预置一个旧文件,用于验证「保留 vs 覆盖」的联动行为
    echo 'OLD-DIST-FILE' > "$VOL_DIST/old.txt"

    # 截掉末尾 `exec ...` 行,生成只跑同步段落的可执行脚本(仍在 set -e 下)
    SYNC_SH="$WORK/entrypoint-sync.sh"
    sed '/^exec /d' "$ENTRYPOINT" > "$SYNC_SH"

    OUTPUT="$(
        REVIEW_IMAGE_BIN="$IMG_BIN_DIR/review-engine" \
        REVIEW_VOL_BIN="$VOL_BIN_DIR/review-engine" \
        REVIEW_IMAGE_DIST="$IMG_DIST" \
        REVIEW_VOL_DIST="$VOL_DIST" \
        bash "$SYNC_SH" 2>&1
    )"
}

cleanup() {
    local w
    for w in "${WORKS[@]:-}"; do
        rm -rf "$w"
    done
}
trap cleanup EXIT

# ── 用例 1: 卷空 → 首次启动从镜像同步(二进制 + dist) ──────────────────────
echo "== 用例 1: 卷内无二进制 → 从镜像同步(首次启动) =="
prepare v0.9.14 NONE 1
check "输出含首次启动提示" "printf '%s' \"\$OUTPUT\" | grep -q '首次启动'"
check "卷二进制已生成且版本为镜像 v0.9.14" "test -f \"\$VOL_BIN_DIR/review-engine\" && grep -q 'v0.9.14' \"\$VOL_BIN_DIR/review-engine\""
check "dist 已同步(index.html 存在)" "test -f \"\$VOL_DIST/index.html\""
check "dist 内容为镜像新内容" "grep -q 'NEW-DIST' \"\$VOL_DIST/index.html\""
echo

# ── 用例 2: 镜像 v0.9.14 > 卷 v0.9.12 → 覆盖二进制并联动同步 dist ────────
echo "== 用例 2: 镜像 v0.9.14 > 卷 v0.9.12 → 覆盖二进制 + 联动同步 dist =="
prepare v0.9.14 v0.9.12 1
check "输出含「覆盖」" "printf '%s' \"\$OUTPUT\" | grep -q '覆盖'"
check "卷二进制已更新为 v0.9.14" "grep -q 'v0.9.14' \"\$VOL_BIN_DIR/review-engine\""
check "卷二进制不再是 v0.9.12" "! grep -q 'v0.9.12' \"\$VOL_BIN_DIR/review-engine\""
check "dist/index.html 已联动同步" "test -f \"\$VOL_DIST/index.html\" && grep -q 'NEW-DIST' \"\$VOL_DIST/index.html\""
echo

# ── 用例 3: 卷 v0.9.14 >= 镜像 v0.9.14 → 保留卷,dist 一并保留 ────────────
echo "== 用例 3: 卷 v0.9.14 >= 镜像 v0.9.14 → 保留卷二进制 + dist 保留 =="
prepare v0.9.14 v0.9.14 1
check "输出含「保留」" "printf '%s' \"\$OUTPUT\" | grep -q '保留'"
check "卷二进制仍为 v0.9.14" "grep -q 'v0.9.14' \"\$VOL_BIN_DIR/review-engine\""
check "dist 未同步(index.html 不存在)" "! test -f \"\$VOL_DIST/index.html\""
check "dist 旧文件 old.txt 仍在" "test -f \"\$VOL_DIST/old.txt\""
echo

# ── 用例 4a: 镜像 --version 输出非预期 → 保留卷 + WARN ─────────────────────
echo "== 用例 4a: 镜像 --version 输出非预期 → 保留卷 + WARN =="
prepare BAD v0.9.12 1
check "输出含 WARN" "printf '%s' \"\$OUTPUT\" | grep -q 'WARN'"
check "卷二进制保留 v0.9.12" "grep -q 'v0.9.12' \"\$VOL_BIN_DIR/review-engine\""
check "dist 保留(未同步)" "! test -f \"\$VOL_DIST/index.html\""
echo

# ── 用例 4b: 卷 --version 退出码非 0 → 保留卷 + WARN ───────────────────────
echo "== 用例 4b: 卷 --version 退出码非 0 → 保留卷 + WARN =="
prepare v0.9.14 EXIT1 1
check "输出含 WARN" "printf '%s' \"\$OUTPUT\" | grep -q 'WARN'"
check "卷二进制保留(内容未变,仍为 EXIT1 桩)" "grep -q 'v9.9.9' \"\$VOL_BIN_DIR/review-engine\""
check "dist 保留(未同步)" "! test -f \"\$VOL_DIST/index.html\""
echo

# ── 用例 5: 镜像 v0.9.10 < 卷 v0.9.14 → 不降级,保留卷 ──────────────────────
echo "== 用例 5: 镜像 v0.9.10 < 卷 v0.9.14 → 不降级,保留卷 =="
prepare v0.9.10 v0.9.14 1
check "输出含「保留」" "printf '%s' \"\$OUTPUT\" | grep -q '保留'"
check "卷二进制仍为 v0.9.14(未降级)" "grep -q 'v0.9.14' \"\$VOL_BIN_DIR/review-engine\""
check "dist 保留(未同步)" "! test -f \"\$VOL_DIST/index.html\""
echo

echo "== 结果: $PASS passed, $FAIL failed =="
[ "$FAIL" -eq 0 ] || exit 1
