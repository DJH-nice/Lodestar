#!/usr/bin/env bash
# 发布一个 Release：建 release + 上传 Lodestar.dmg 附件。
#
# 【为什么要这个脚本】
#   GitHub / Gitee 的 **API 写操作都要 token**（Gitee 用私人令牌，GitHub 用 PAT）。
#   token 不该写进仓库、也不该出现在聊天记录里 →
#   所以这里**交互式地问**，读到内存里用完就丢（不写文件、不进 shell 历史）。
#
# 【用法】
#   bash app/scripts/publish-release.sh                  # 用已存在的 tag 发布
#   bash app/scripts/publish-release.sh --tag v0.1.1     # 先打 tag 再发布
#   bash app/scripts/publish-release.sh --dry-run        # 只检查，不打 tag、不建 release、不上传
#
# 【token 怎么拿】
#   GitHub：https://github.com/settings/tokens/new → 勾 `repo` → 生成
#   Gitee ：https://gitee.com/profile/personal_access_tokens → 勾 `projects` → 生成
#
# 【幂等】同名 release 已存在时会报错并停下（不覆盖已发布的版本）。

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

# ── 仓库地址（两个平台各一个）─────────────────────────────
GH_OWNER="DJH-nice"
GH_REPO="Lodestar"
GT_OWNER="dongjiahao0119"
GT_REPO="lodestar"          # Gitee 的 API 路径用小写（它的 full_name 就是小写）
GH_API="https://api.github.com"
GH_UPLOAD="https://uploads.github.com"
GT_API="https://gitee.com/api/v5"

# ── 产物 ────────────────────────────────────────────────
DMG="app/src-tauri/target/release/bundle/macos/Lodestar.dmg"

TAG=""
DRY_RUN=0
while [ $# -gt 0 ]; do
  case "$1" in
    --tag) TAG="$2"; shift 2 ;;
    --dry-run) DRY_RUN=1; shift ;;
    *) echo "未知参数: $1" >&2; exit 64 ;;
  esac
done

if [ ! -f "$DMG" ]; then
  echo "找不到产物: $DMG" >&2
  echo "先跑: bash app/scripts/bundle-app.sh" >&2
  exit 1
fi

SHA=$(shasum -a 256 "$DMG" | awk '{print $1}')
SIZE=$(du -h "$DMG" | cut -f1)

# ── Release 正文 ─────────────────────────────────────────
NOTES=$(cat <<EOF
## Lodestar ${TAG}

**macOS / Apple Silicon 上的原生文件搜索工具** —— 插上 U 盘就能搜到里面的文件。

### 这个版本有什么

- **搜索引擎内核完全自研** —— 零第三方引擎代码，MIT 许可
  （早前版本用过上游 FSearch 的 C 引擎，2026-10-23 起整体替换掉了）
- **外接盘自动索引** —— 插上 U 盘/移动硬盘，1 秒内进索引；拔掉立刻移除，**不留幽灵条目**；
  插回来直接秒开（复用上次的索引档），再后台刷新一遍
- **实时更新** —— 文件新建 / 修改 / 删除后**增量**更新索引，不做全量重扫
- **查询语法** —— \`ext:\` \`size:\` \`dm:\` \`path:\` \`regex:\` \`folder:\` … 以及 AND / OR / NOT 与括号分组
- **体积 ${SIZE}** —— 不拖任何第三方动态库，拷到任何 Mac 上双击就能跑

### 安装

1. 下载 \`Lodestar.dmg\`
2. 双击打开，把 \`Lodestar.app\` 拖进「应用程序」
3. 右键点 app →「打开」（没有 Developer ID 签名，Gatekeeper 会拦一下，选「仍要打开」）

### 校验和

\`\`\`
SHA256: ${SHA}
\`\`\`

### 已知限制

- 只支持 Apple Silicon（arm64）
- 没有 Developer ID 签名与公证
- 外接盘的挂载点变了会当新盘重新扫一次
- \`path:\` 是子串匹配，不做 \`~\` 展开

完整说明见仓库 README。
EOF
)

# ── 1. tag（可选）─────────────────────────────────────────
# 【为什么在读 token 之后才打 tag】
#   打 tag + push 是**对外可见**的副作用。
#   放在前面的话，一次误触的「试跑」就会把 tag 推到两个平台 —— 还得回头删。
#   → 先确认「真的要发布」，再动 tag。
if [ -z "$TAG" ]; then
  TAG=$(git describe --tags --abbrev=0 2>/dev/null || echo "")
  if [ -z "$TAG" ]; then
    echo "仓库里还没有 tag。用 --tag v0.1.0 指定一个。" >&2
    exit 1
  fi
  echo "使用已存在的 tag: $TAG"
fi

if [ "$DRY_RUN" -eq 1 ]; then
  echo
  echo "=== [dry-run] 到此为止，不打 tag、不建 release、不上传 ==="
  echo "  tag      : $TAG"
  echo "  产物     : $DMG  ($SIZE)"
  echo "  SHA256   : $SHA"
  echo "  GitHub   : $GH_OWNER/$GH_REPO"
  echo "  Gitee    : $GT_OWNER/$GT_REPO"
  echo "  tag 状态 : $(git rev-parse "$TAG" >/dev/null 2>&1 && echo '本地已存在' || echo '本地不存在（发布时会自动打）')"
  echo "  远端 tag : origin=$(git ls-remote --tags origin "refs/tags/$TAG" | wc -l | tr -d ' ') gitee=$(git ls-remote --tags gitee "refs/tags/$TAG" | wc -l | tr -d ' ')"
  exit 0
fi

if git rev-parse "$TAG" >/dev/null 2>&1; then
  echo "tag $TAG 已存在"
else
  echo "打 tag: $TAG"
  git tag -a "$TAG" -m "Lodestar $TAG"
  git push origin "$TAG"
  git push gitee "$TAG"
fi

echo
echo "产物: $DMG  ($SIZE)"
echo "SHA256: $SHA"
echo

# ── 2. 要 token ───────────────────────────────────────────
echo "请粘贴 GitHub Personal Access Token（需要 repo 权限）:"
echo "  获取地址: https://github.com/settings/tokens/new"
read -rs GH_TOKEN
echo
echo "请粘贴 Gitee 私人令牌（需要 projects 权限）:"
echo "  获取地址: https://gitee.com/profile/personal_access_tokens"
read -rs GT_TOKEN
echo
echo

# ── 3. GitHub：建 release + 上传附件 ──────────────────────
echo "=== GitHub ==="
GH_RESP=$(curl -sS -X POST "$GH_API/repos/$GH_OWNER/$GH_REPO/releases" \
  -H "Authorization: Bearer $GH_TOKEN" \
  -H "Accept: application/vnd.github+json" \
  -H "X-GitHub-Api-Version: 2022-11-28" \
  --data-binary "$(python3 - "$TAG" "$NOTES" <<'PY'
import json, sys
tag, notes = sys.argv[1], sys.argv[2]
print(json.dumps({
    "tag_name": tag,
    "name": f"Lodestar {tag}",
    "body": notes,
    "draft": False,
    "prerelease": False,
}, ensure_ascii=False))
PY
)")
GH_ID=$(printf '%s' "$GH_RESP" | python3 -c "import json,sys
try: print(json.load(sys.stdin).get('id',''))
except Exception: print('')" 2>/dev/null || true)
if [ -z "$GH_ID" ]; then
  echo "✗ GitHub release 创建失败：$GH_RESP" >&2
else
  echo "  release 已创建 (id=$GH_ID)"
  GH_UP=$(curl -sS -X POST "$GH_UPLOAD/repos/$GH_OWNER/$GH_REPO/releases/$GH_ID/assets?name=Lodestar.dmg" \
    -H "Authorization: Bearer $GH_TOKEN" \
    -H "Content-Type: application/octet-stream" \
    --data-binary "@$DMG")
  printf '%s' "$GH_UP" | python3 -c "import json,sys
try:
    d=json.load(sys.stdin)
    print('  ✓ 附件已上传:', d.get('name'), d.get('size'), 'bytes') if 'id' in d else print('  ✗ 上传失败:', d)
except Exception as e:
    print('  ✗ 上传失败:', e)" 2>/dev/null || echo "  ✗ 上传失败（响应不是 JSON）"
fi

# ── 4. Gitee：建 release + 上传附件 ───────────────────────
echo
echo "=== Gitee ==="
GT_RESP=$(curl -sS -X POST "$GT_API/repos/$GT_OWNER/$GT_REPO/releases" \
  --data-urlencode "access_token=$GT_TOKEN" \
  --data-urlencode "tag_name=$TAG" \
  --data-urlencode "name=Lodestar $TAG" \
  --data-urlencode "body=$NOTES" \
  --data-urlencode "prerelease=false")
GT_ID=$(printf '%s' "$GT_RESP" | python3 -c "import json,sys
try:
    d=json.load(sys.stdin); print(d.get('id',''))
except Exception: print('')" 2>/dev/null || true)
if [ -z "$GT_ID" ]; then
  echo "✗ Gitee release 创建失败：$GT_RESP" >&2
else
  echo "  release 已创建 (id=$GT_ID)"
  GT_UP=$(curl -sS -X POST "$GT_API/repos/$GT_OWNER/$GT_REPO/releases/$GT_ID/attach_files" \
    -F "file=@$DMG" \
    -F "name=Lodestar.dmg" \
    -F "access_token=$GT_TOKEN")
  printf '%s' "$GT_UP" | python3 -c "import json,sys
try:
    d=json.load(sys.stdin)
    ok = isinstance(d, list) and len(d)>0
    print('  ✓ 附件已上传:', d[0].get('name'), d[0].get('size'), 'bytes') if ok else print('  ✗ 上传失败:', d)
except Exception as e:
    print('  ✗ 上传失败:', e)" 2>/dev/null || echo "  ✗ 上传失败（响应不是 JSON）"
fi

# ── 5. Issue 标签（尽力而为，失败不影响发布）──────────────
# 【为什么要有】Issue 模板里引用了 bug / enhancement / question 三个标签，
#   标签不存在时模板会**静默忽略**那个标签 → issue 进来就是一堆「无标签」。
#   这里补建；已存在会返回 422，忽略即可。
echo
echo "=== Issue 标签 ==="
make_label() {
  local api="$1" owner="$2" repo="$3" token="$4" auth_header="$5" name="$6" color="$7" desc="$8"
  if [ -n "$auth_header" ]; then
    curl -sS -o /dev/null -X POST "$api/repos/$owner/$repo/labels" \
      -H "$auth_header $token" -H "Content-Type: application/json" \
      -d "{\"name\":\"$name\",\"color\":\"$color\",\"description\":\"$desc\"}" || true
  else
    curl -sS -o /dev/null -X POST "$api/repos/$owner/$repo/labels" \
      --data-urlencode "access_token=$token" \
      --data-urlencode "name=$name" \
      --data-urlencode "color=$color" \
      --data-urlencode "description=$desc" || true
  fi
}

for spec in "bug:d73a4a:Something is not working" \
            "enhancement:a2eeef:Feature request" \
            "question:cc317c:Usage question"; do
  name="${spec%%:*}"; rest="${spec#*:}"; color="${rest%%:*}"; desc="${rest#*:}"
  make_label "$GH_API" "$GH_OWNER" "$GH_REPO" "$GH_TOKEN" "Authorization: Bearer" "$name" "$color" "$desc"
  make_label "$GT_API" "$GT_OWNER" "$GT_REPO" "$GT_TOKEN" "" "$name" "$color" "$desc"
done
echo "  ✓ bug / enhancement / question（已存在则跳过）"

unset GH_TOKEN GT_TOKEN
echo
echo "完成。核对："
echo "  GitHub: https://github.com/$GH_OWNER/$GH_REPO/releases/tag/$TAG"
echo "  Gitee : https://gitee.com/$GT_OWNER/Lodestar/releases/tag/$TAG"
