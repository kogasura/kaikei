#!/usr/bin/env bash
#
# 自動レビューのコメントを PR に1つだけ保つ（作る／書き換える／解消にする）。
#
# **判断と機構を分ける。** 何を指摘するかはモデルが決めるが、コメントを
# 出し分ける手順は決定的でよい。モデルに `gh api` を組み立てさせると、
# 引数を1つ間違えただけで黙って別のことをする。ここに固定しておく。
#
# 使い方:
#   .github/scripts/post_review_comment.sh <PR番号> <本文のファイル>
#   .github/scripts/post_review_comment.sh <PR番号> --clear
#
# `--clear` は「前回の指摘が解消された」状態にする。**前回のコメントが
# 無ければ何もしない**——毎回「問題ありません」と言うコメントは要らない。
#
# 環境変数: GH_TOKEN（gh の認証）、GITHUB_REPOSITORY（owner/repo）

set -euo pipefail

MARKER='<!-- real-data-review -->'

PR="${1:?PR番号を渡してください}"
BODY_ARG="${2:?本文のファイルか --clear を渡してください}"
REPO="${GITHUB_REPOSITORY:?GITHUB_REPOSITORY が未設定です}"

# 目印の付いた既存コメントを探す。**push のたびに新しいコメントが積み上がると
# 読まれなくなる**ので、あれば書き換える。
existing=$(gh api "repos/$REPO/issues/$PR/comments" --paginate \
  --jq "map(select(.body != null and (.body | contains(\"$MARKER\")))) | .[0].id // empty")

if [ "$BODY_ARG" = "--clear" ]; then
  if [ -z "$existing" ]; then
    echo "前回のコメントは無いので、何もしません"
    exit 0
  fi
  body=$(printf '%s\n## 実データが混ざっていないか（自動レビュー）\n\n前回の指摘は、最新の差分では見当たりませんでした。\n' "$MARKER")
  gh api -X PATCH "repos/$REPO/issues/comments/$existing" -f "body=$body" >/dev/null
  echo "前回の指摘を「解消」に書き換えました"
  exit 0
fi

[ -f "$BODY_ARG" ] || { echo "本文のファイルがありません: $BODY_ARG" >&2; exit 1; }

# 目印が無ければ足す（モデルが書き忘れても1コメントに保てるように）。
if grep -qF "$MARKER" "$BODY_ARG"; then
  body=$(cat "$BODY_ARG")
else
  body=$(printf '%s\n%s' "$MARKER" "$(cat "$BODY_ARG")")
fi

if [ -n "$existing" ]; then
  gh api -X PATCH "repos/$REPO/issues/comments/$existing" -f "body=$body" >/dev/null
  echo "既存のコメントを更新しました（id=$existing）"
else
  gh api -X POST "repos/$REPO/issues/$PR/comments" -f "body=$body" \
    --jq '.html_url'
fi
