#!/usr/bin/env bash
#
# teardown.sh — review-engine E2E 环境一键清理(teardown 契约, v0.9.50)
#
# 每轮 E2E 测试结束后必须运行本脚本,把测试床恢复到「干净基线」:
#   1. gitlab-ee-testbed:关闭 review-lab/e2e-security 所有 open MR(不 merge)
#   2. gitlab-ee-testbed:删除项目下 e2e/* 测试分支
#   3. gitlab-ee-testbed:revoke 所有 e2e-*/​*accept* 命名的临时 PAT
#   4. 本机:删除 /tmp/e2e-* 下指向测试床的 git 克隆
#   5. webbridge(127.0.0.1:10086):关闭 reng/e2e 测试标签 session
#
# 红线(任何模式都不得触碰):
#   - 不删除 review-lab/e2e-security 项目本体(保留的固定回归测试床)
#   - 不动 main 及非 e2e/* 分支
#   - 不动 root 长期 token(见 KEEP_TOKEN_NAMES,如 review-engine-test-token)
#   - 不动 gitlab-ee-testbed / review-engine-preview 容器配置
#   - webbridge 只关闭「全部标签都匹配测试模式」的 session;
#     混入非测试标签的 session 跳过并告警 —— 用户自己的浏览标签一概不动
#
# 用法:
#   tests/e2e/teardown.sh           # dry-run(默认):只打印将执行的动作,不做任何修改
#   tests/e2e/teardown.sh --yes     # 实跑
#
# 幂等:重复运行安全;环境已干净时各步骤为 no-op,退出码 0。
# 容错:任一步骤失败不中断后续步骤,最后汇总,退出码 = 失败步骤数(0 为全部成功)。
#
# 已知环境适配(2026-09-02 实测):
#   - 该 GitLab 版本 merge_requests.state 列已迁移为 state_id,查询须用 .opened scope
#   - ServiceResponse 无 #error 方法,错误信息用 #message
#   - webbridge daemon 无 groups / session-list API,session 名从 daemon 日志提取后逐一探测

set -uo pipefail

# ============================== 集中配置项 ==============================
CONTAINER_NAME="gitlab-ee-testbed"            # GitLab 测试床容器
PROJECT_PATH="review-lab/e2e-security"        # 固定测试床项目(绝不删除)
BRANCH_REGEX='^e2e/'                          # 可删除的测试分支名模式
PAT_NAME_LIKES=('e2e-%' '%accept%')           # 临时 PAT 命名模式(SQL ILIKE)
KEEP_TOKEN_NAMES=('review-engine-test-token') # 长期 token 白名单,永不 revoke
CLONE_GLOB="/tmp/e2e-*"                       # 本地临时克隆目录模式
TESTBED_URL_MARKERS=('localhost:8929' '127.0.0.1:8929')  # 克隆 origin 归属测试床的判定
WEBBRIDGE_URL="http://127.0.0.1:10086"        # webbridge daemon
WEBBRIDGE_LOG="${WEBBRIDGE_LOG:-$HOME/.kimi-webbridge/logs/daemon.log}"
TAB_URL_MARKERS=('localhost:18080' 'localhost:8929')     # 测试标签 URL 判定
TAB_GROUP_REGEX='reng|e2e'                    # 测试标签组名判定(大小写不敏感)
# ========================================================================

DRY_RUN=1
[ "${1:-}" = "--yes" ] && DRY_RUN=0

FAILURES=0
ACTIONS=0

say()  { printf '%s\n' "$*"; }
act()  { ACTIONS=$((ACTIONS + 1)); if [ "$DRY_RUN" -eq 1 ]; then say "  [dry-run] $*"; else say "  [exec] $*"; fi; }
fail() { FAILURES=$((FAILURES + 1)); say "  [FAIL] $*" >&2; }
mode() { [ "$DRY_RUN" -eq 1 ] && echo "DRY-RUN(加 --yes 实跑)" || echo "EXECUTE"; }

say "== review-engine E2E teardown ==  mode: $(mode)"

# ---------------- 步骤 1-3:GitLab 测试床(MR / 分支 / PAT) ----------------
step_gitlab() {
  say "-- [1-3] GitLab 测试床: $PROJECT_PATH @ $CONTAINER_NAME"
  if ! docker ps --format '{{.Names}}' | grep -qx "$CONTAINER_NAME"; then
    fail "容器 $CONTAINER_NAME 未运行,跳过 GitLab 清理"
    return
  fi

  local keep_cond="" name
  for name in "${KEEP_TOKEN_NAMES[@]}"; do
    keep_cond="$keep_cond AND name NOT ILIKE '$name'"
  done
  local pat_likes_ruby
  pat_likes_ruby=$(printf "'%s'," "${PAT_NAME_LIKES[@]}")
  pat_likes_ruby="[${pat_likes_ruby%,}]"

  # 单条 rails runner 完成 枚举(dry-run)/执行(--yes);MODE 经环境变量传入
  local out rc
  out=$(docker exec -e TEARDOWN_MODE="$DRY_RUN" "$CONTAINER_NAME" gitlab-rails runner "
project = Project.find_by_full_path('$PROJECT_PATH')
if project.nil?
  puts 'WARN project-not-found'
else
  mrs      = project.merge_requests.opened.order(:iid).to_a
  branches = project.repository.branch_names.grep(Regexp.new('$BRANCH_REGEX'))
  pats     = PersonalAccessToken.where(\"name ILIKE ANY (ARRAY${pat_likes_ruby})$keep_cond\").where(revoked: false).to_a
  dry = ENV['TEARDOWN_MODE'] == '1'
  mrs.each do |mr|
    if dry
      puts \"MR !#{mr.iid} would-close (#{mr.title})\"
    else
      mr.close!
      puts \"MR !#{mr.iid} -> #{mr.reload.state}\"
    end
  end
  branches.each do |b|
    if dry
      puts \"branch #{b} would-delete\"
    else
      r = Branches::DeleteService.new(project, project.first_owner).execute(b)
      puts r.success? ? \"branch #{b} -> deleted\" : \"branch #{b} -> FAILED: #{r.message}\"
    end
  end
  pats.each do |t|
    if dry
      puts \"PAT #{t.name} (user=#{t.user.username}) would-revoke\"
    else
      t.revoke!
      puts \"PAT #{t.name} -> revoked=#{t.reload.revoked}\"
    end
  end
  puts 'DONE'
end
" 2>&1)
  rc=$?

  if [ $rc -ne 0 ] || ! printf '%s' "$out" | grep -q '^DONE$'; then
    fail "gitlab-rails runner 异常 (rc=$rc): $(printf '%s' "$out" | tail -3 | tr '\n' ' ')"
    return
  fi
  if printf '%s' "$out" | grep -q '^WARN project-not-found'; then
    say "  [warn] 项目 $PROJECT_PATH 不存在(非本测试床?),跳过"
    return
  fi
  local n=0 line
  while IFS= read -r line; do
    case "$line" in
      DONE) ;;
      *FAILED*) fail "$line" ;;
      *) act "$line"; n=$((n + 1)) ;;
    esac
  done <<< "$out"
  [ "$n" -eq 0 ] && say "  [ok] 已干净:无 open MR / e2e 分支 / 待撤销临时 PAT"
}

# ---------------- 步骤 4:删除本地 /tmp/e2e-* 克隆 ----------------
step_clones() {
  say "-- [4] 本地临时克隆: $CLONE_GLOB"
  shopt -s nullglob
  local dirs=()
  local d
  for d in $CLONE_GLOB; do dirs+=("$d"); done
  shopt -u nullglob
  if [ ${#dirs[@]} -eq 0 ]; then
    say "  [ok] 无匹配目录,no-op"
    return
  fi
  local dir origin marker matched
  for dir in "${dirs[@]}"; do
    if [ ! -d "$dir" ]; then
      say "  [skip] $dir 存在但不是目录(非克隆,可能是测试残留文件),按红线不动,请人工确认"
      continue
    fi
    if ! git -C "$dir" rev-parse --git-dir >/dev/null 2>&1; then
      say "  [skip] $dir 不是 git repo,不动(红线:只删确认的测试克隆)"
      continue
    fi
    origin=$(git -C "$dir" remote get-url origin 2>/dev/null || true)
    matched=0
    for marker in "${TESTBED_URL_MARKERS[@]}"; do
      case "$origin" in *"$marker"*) matched=1 ;; esac
    done
    if [ "$matched" -eq 0 ]; then
      say "  [skip] $dir origin($origin) 不指向测试床,不动"
      continue
    fi
    act "rm -rf $dir (origin: ${origin%%@*}@***)"
    if [ "$DRY_RUN" -eq 0 ]; then
      rm -rf -- "$dir" && say "  [ok] 已删除 $dir" || fail "删除 $dir 失败"
    fi
  done
}

# ---------------- 步骤 5:关闭 webbridge 测试标签 session ----------------
step_webbridge() {
  say "-- [5] webbridge 测试标签 session: $WEBBRIDGE_URL"
  if ! curl -s -m 3 "$WEBBRIDGE_URL/status" >/dev/null 2>&1; then
    fail "webbridge daemon 不可达,跳过浏览器清理"
    return
  fi
  if [ ! -r "$WEBBRIDGE_LOG" ]; then
    fail "无法读取 $WEBBRIDGE_LOG(无法枚举历史 session),跳过"
    return
  fi

  # daemon 无 session-list API:从日志提取全部历史 session 名,逐一 list_tabs 探测存活标签
  local sessions s resp
  sessions=$(grep -oE '"session":"[^"]+"' "$WEBBRIDGE_LOG" 2>/dev/null \
             | sed 's/"session":"//; s/"$//' | grep -v '^$' | sort -u)
  local closed_any=0
  for s in $sessions; do
    resp=$(curl -s -m 5 -X POST "$WEBBRIDGE_URL/command" \
             -H 'Content-Type: application/json' \
             -d "{\"action\":\"list_tabs\",\"session\":\"$s\"}" 2>/dev/null) || continue
    # python3 判定:该 session 是否有存活标签,且是否「全部」匹配测试模式
    local verdict
    verdict=$(printf '%s' "$resp" | python3 -c '
import json, sys, re
try:
    tabs = json.load(sys.stdin).get("data", {}).get("tabs", [])
except Exception:
    print("error"); sys.exit()
if not tabs:
    print("empty"); sys.exit()
url_markers = sys.argv[1].split(",")
group_re = re.compile(sys.argv[2], re.I)
def is_test(t):
    u = t.get("url", ""); g = t.get("groupTitle", "") or ""
    return any(m in u for m in url_markers) or bool(group_re.search(g))
print("all-test" if all(is_test(t) for t in tabs) else "mixed:%d/%d" % (
    sum(1 for t in tabs if is_test(t)), len(tabs)))
' "$(IFS=,; echo "${TAB_URL_MARKERS[*]}")" "$TAB_GROUP_REGEX")

    case "$verdict" in
      empty|error) ;;
      all-test)
        act "close_session \"$s\""
        if [ "$DRY_RUN" -eq 0 ]; then
          local cresp
          cresp=$(curl -s -m 5 -X POST "$WEBBRIDGE_URL/command" \
                    -H 'Content-Type: application/json' \
                    -d "{\"action\":\"close_session\",\"session\":\"$s\"}" 2>/dev/null)
          if printf '%s' "$cresp" | grep -q '"ok":true'; then
            say "  [ok] $s -> $(printf '%s' "$cresp" | python3 -c 'import json,sys; print("closed=%s" % json.load(sys.stdin)["data"].get("closed","?"))' 2>/dev/null || echo closed)"
            closed_any=1
          else
            fail "close_session $s 失败: ${cresp:0:120}"
          fi
        else
          closed_any=1
        fi
        ;;
      mixed:*)
        say "  [skip] session \"$s\" 含非测试标签($verdict),按红线跳过,请人工核对"
        ;;
    esac
  done
  [ "$closed_any" -eq 0 ] && say "  [ok] 无存活测试标签 session,no-op"
}

step_gitlab
step_clones
step_webbridge

say "== 汇总 =="
say "  动作数: $ACTIONS   失败步骤数: $FAILURES   模式: $(mode)"
if [ "$FAILURES" -gt 0 ]; then
  say "  结果: 有失败,见上方 [FAIL] 行(失败不中断是本脚本的设计)"
else
  say "  结果: 全部成功$([ "$DRY_RUN" -eq 1 ] && echo '(dry-run,未实际修改)')"
fi
exit "$FAILURES"
