#!/usr/bin/env python3
"""PR の差分を Claude に読ませ、**指摘があるときだけ** PR にコメントする。

# なぜ正規表現の検査（`no_real_data.py`）と別に要るのか

正規表現は**固有名詞をまったく拾えない**。「株式会社ビーテック」が実在かどうかを
denylist 無しにパターンで判定する方法は無く、denylist は置かないと決めている
（CLAUDE.md §15）。ところが実際に見つかった漏洩は、種類でいえば大半が固有名詞
だった——事業者名・取引先名・契約サービス名・OS のユーザー名。そこが穴である。

言語モデルは「これは実在の社名に見える」を判断できる。ここが埋めるのはその穴で
あって、金額の書き方は正規表現側の担当である。

# **落とさない**

このジョブは必須チェックにしない。判定がブレるものを門番に据えると、落ちても
「またか」と再実行されるようになり、本物の検出まで一緒に無視される
（CLAUDE.md §13 の必須チェックは決定的なものだけに絞る）。

だから、
  - 指摘があるとき **だけ** コメントする（無ければ黙る）
  - API が落ちていても、鍵が無くても、終了コードは 0
  - 断定しない。**確かめるべき箇所を人に渡すところまで**が仕事である

# 使い方

    ANTHROPIC_API_KEY=... GITHUB_TOKEN=... \\
      python3 .github/scripts/real_data_review.py --diff origin/main

    # 手元で試すとき（コメントは投げず、標準出力に出すだけ）
    python3 .github/scripts/real_data_review.py --diff origin/main --dry-run
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import urllib.error
import urllib.request

MODEL = "claude-sonnet-5"

# 1回のリクエストに載せる差分の大きさ（文字）。**切り詰めない。**
# 超えるときはファイル単位で分けて複数回に渡す。
CHUNK_CHARS = 120_000

# コメントを1つに保つための目印。次回以降はこれを探して**書き換える**
# （push のたびに新しいコメントが積み上がると読まれなくなる）。
MARKER = "<!-- real-data-review -->"

SYSTEM = """\
あなたは公開リポジトリのレビュアーである。個人事業主向けの会計ソフトのリポジトリで、
開発者が実際の帳簿で検算しながら書いているため、**実在のデータがそのまま
コミットされる事故**が過去に起きている。

このリポジトリの規約（CLAUDE.md §14 / §15）:

- 事業者は名前を付けず「検証帳簿」と呼ぶ
- 取引先は `株式会社ABC` / `DEF株式会社` のような明らかな架空名
- 摘要は `カ)サンプル シヨウジ` のような架空の店名
- 取り込み元は `example_bank` / `example_card`（実在の銀行・カード名を使わない）
- 金額は桁と演算関係だけを本物に似せた架空値
- 件数は「大半が」「数百件」のような規模の言い方

あなたの担当は**固有名詞**である。別途、正規表現の検査が「鍵・トークン・メール・
住所・ローカルパス」と「『実帳簿』の語と具体的な数の同居」を見ているので、そちらは
任せてよい。あなたにしか判断できないのは「この文字列は実在の会社・サービス・人・
場所に見えるか」である。

# 指摘すべきもの

- 実在しそうな会社名・屋号・サービス名・製品名・銀行名・カード名
- 人名らしき文字列（ローマ字のユーザー名を含む）
- 実在しそうな地名・建物名
- 一組の帳簿から取ってきたように見える金額の集まり（不自然に細かい額が揃っている）
- 帳簿の規模が分かる件数

# 指摘してはいけないもの

- 明らかに架空と分かる置き換え済みの名前（株式会社ABC、DEF株式会社、サンプル◯◯、
  example_bank など）
- 公開資料への参照（国税庁の通達番号・URL、弥生のサポートページ番号、
  法令の条番号、その引用にある計算例の数値）
- 技術的な数（SQLSTATE、マイグレーション番号、エラーコード、バイト数、行数、
  ポート番号、バージョン、UUID、ハッシュ）
- ソフトウェア製品名・ライブラリ名（PostgreSQL、sqlx、Rust など）。これらは
  実装の話であって帳簿の中身ではない
- 丸い数（1,000 や 100,000 のような、いかにもテスト用の値）

# 姿勢

**断定しない。** あなたは「実額かどうか」を知り得ない。できるのは
「これは実在のものに見えるので確かめる価値がある」と言うところまでである。

**黙ることを恐れない。** 疑わしいものが無ければ findings を空で返す。無理に
何か挙げるくらいなら、何も挙げないほうが良い。毎回何か言うレビュアーは読まれなく
なり、本当の指摘まで一緒に読み飛ばされる。

confidence は、そのまま人に見せる価値があるかで決める。
- high … ほぼ実在のものだと思う
- medium … 実在かもしれない。確かめる価値がある
- low … 気になるが、たぶん問題ない
"""

SCHEMA = {
    "type": "object",
    "properties": {
        "findings": {
            "type": "array",
            "items": {
                "type": "object",
                "properties": {
                    "file": {"type": "string", "description": "差分中のファイルパス"},
                    "excerpt": {
                        "type": "string",
                        "description": "問題だと思う箇所を、差分からそのまま1行で抜き出す",
                    },
                    "kind": {
                        "type": "string",
                        "enum": [
                            "事業者名", "取引先名", "サービス名", "銀行名",
                            "人名", "地名", "金額", "件数", "その他",
                        ],
                    },
                    "reason": {
                        "type": "string",
                        "description": "なぜ実在に見えるのかを1〜2文で。断定しない",
                    },
                    "confidence": {"type": "string", "enum": ["high", "medium", "low"]},
                },
                "required": ["file", "excerpt", "kind", "reason", "confidence"],
                "additionalProperties": False,
            },
        }
    },
    "required": ["findings"],
    "additionalProperties": False,
}


def warn(message: str) -> None:
    print(f"::warning::{message}")


def diff_chunks(base: str) -> list[str]:
    """差分をファイル単位に割り、`CHUNK_CHARS` に収まる塊にまとめる。

    **切り詰めない。** 収まらなければ回数を増やす。差分の後ろ半分を黙って
    捨てると、「見た」と言いながら見ていないことになる。
    """
    try:
        diff = subprocess.check_output(
            ["git", "diff", f"{base}...HEAD"], text=True, stderr=subprocess.DEVNULL
        )
    except subprocess.CalledProcessError:
        return []

    files, current = [], []
    for line in diff.splitlines(keepends=True):
        if line.startswith("diff --git ") and current:
            files.append("".join(current))
            current = []
        current.append(line)
    if current:
        files.append("".join(current))

    chunks, buffer = [], ""
    for section in files:
        if buffer and len(buffer) + len(section) > CHUNK_CHARS:
            chunks.append(buffer)
            buffer = ""
        buffer += section
    if buffer:
        chunks.append(buffer)
    return chunks


def review(client, chunk: str) -> list[dict]:
    response = client.messages.create(
        model=MODEL,
        max_tokens=16000,
        system=SYSTEM,
        thinking={"type": "adaptive"},
        output_config={
            "effort": "medium",
            "format": {"type": "json_schema", "schema": SCHEMA},
        },
        messages=[
            {
                "role": "user",
                "content": (
                    "次の差分を読んで、実在のデータが混ざっていないか見てほしい。\n"
                    "追加された行（`+` で始まる行）だけを見ること。削除された行は、"
                    "取り除いている最中なので指摘しなくてよい。\n\n"
                    f"```diff\n{chunk}\n```"
                ),
            }
        ],
    )
    text = next((b.text for b in response.content if b.type == "text"), "")
    return json.loads(text).get("findings", [])


def render(findings: list[dict]) -> str:
    lines = [
        MARKER,
        "## 実データが混ざっていないか（自動レビュー）",
        "",
        "この PR の差分に、**実在のものに見える固有名詞や金額**がありました。"
        "架空値なら、このコメントは無視してかまいません（CLAUDE.md §14 / §15）。",
        "",
        "> このコメントは自動で付いています。**判定ではなく、確かめる価値がある"
        "箇所の提示**です。マージを止めるものではありません。",
        "",
        "| | 種類 | 箇所 | 見つけたもの | なぜ |",
        "|---|---|---|---|---|",
    ]
    mark = {"high": "🔴", "medium": "🟡"}
    for item in findings:
        excerpt = item["excerpt"].strip().replace("|", "\\|").replace("`", "'")
        if len(excerpt) > 70:
            excerpt = excerpt[:70] + "…"
        reason = item["reason"].strip().replace("|", "\\|")
        lines.append(
            f"| {mark.get(item['confidence'], '⚪️')} | {item['kind']} | "
            f"`{item['file']}` | `{excerpt}` | {reason} |"
        )
    lines += [
        "",
        "<sub>固有名詞の検査だけを担当しています。鍵・メール・住所・"
        "「実帳簿」＋数の同居は `no-real-data` ジョブが見ています。</sub>",
    ]
    return "\n".join(lines)


RESOLVED = "\n".join([
    MARKER,
    "## 実データが混ざっていないか（自動レビュー）",
    "",
    "前回の指摘は、最新の差分では見当たりませんでした。",
])


def github(method: str, url: str, token: str, payload: dict | None = None):
    data = json.dumps(payload).encode() if payload is not None else None
    request = urllib.request.Request(url, data=data, method=method)
    request.add_header("Authorization", f"Bearer {token}")
    request.add_header("Accept", "application/vnd.github+json")
    request.add_header("Content-Type", "application/json")
    with urllib.request.urlopen(request) as response:
        return json.loads(response.read() or "null")


def sticky_comment(body: str, repo: str, pr: str, token: str) -> None:
    """目印の付いた既存コメントがあれば書き換え、無ければ作る。"""
    base = f"https://api.github.com/repos/{repo}"
    existing = github("GET", f"{base}/issues/{pr}/comments?per_page=100", token) or []
    for comment in existing:
        if MARKER in (comment.get("body") or ""):
            github("PATCH", f"{base}/issues/comments/{comment['id']}", token,
                   {"body": body})
            print(f"既存のコメントを更新しました: {comment['html_url']}")
            return
    created = github("POST", f"{base}/issues/{pr}/comments", token, {"body": body})
    print(f"コメントしました: {created['html_url']}")


def clear_comment(repo: str, pr: str, token: str) -> None:
    """指摘が無くなったら、前回のコメントだけ「解消」に書き換える。

    **無ければ何もしない。** 毎回「問題ありません」と言うコメントは要らない。
    """
    base = f"https://api.github.com/repos/{repo}"
    existing = github("GET", f"{base}/issues/{pr}/comments?per_page=100", token) or []
    for comment in existing:
        if MARKER in (comment.get("body") or "") and comment["body"] != RESOLVED:
            github("PATCH", f"{base}/issues/comments/{comment['id']}", token,
                   {"body": RESOLVED})
            print("前回の指摘を「解消」に書き換えました")
            return


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--diff", metavar="BASE", required=True)
    parser.add_argument("--dry-run", action="store_true",
                        help="コメントを投げず、標準出力に出すだけ")
    args = parser.parse_args()

    if not os.environ.get("ANTHROPIC_API_KEY"):
        # fork からの PR にはシークレットが渡らない。**黙って通す。**
        warn("ANTHROPIC_API_KEY が無いのでレビューを飛ばします")
        return 0

    try:
        import anthropic
    except ImportError:
        warn("anthropic パッケージが入っていないのでレビューを飛ばします")
        return 0

    chunks = diff_chunks(args.diff)
    if not chunks:
        print("差分が空です")
        return 0

    client = anthropic.Anthropic()
    findings: list[dict] = []
    try:
        for index, chunk in enumerate(chunks, 1):
            print(f"レビュー中 {index}/{len(chunks)}（{len(chunk):,} 文字）")
            findings += review(client, chunk)
    except Exception as error:  # noqa: BLE001 — 落とさないのが仕様
        warn(f"レビューに失敗しました（マージは止めません）: {type(error).__name__}: {error}")
        return 0

    worth_saying = [f for f in findings if f["confidence"] in ("high", "medium")]
    quiet = len(findings) - len(worth_saying)
    print(f"指摘 {len(worth_saying)} 件（confidence low で伏せたもの {quiet} 件）")

    if args.dry_run:
        print(render(worth_saying) if worth_saying else "（コメントすることはありません）")
        return 0

    repo = os.environ.get("GITHUB_REPOSITORY")
    pr = os.environ.get("PR_NUMBER")
    token = os.environ.get("GITHUB_TOKEN")
    if not (repo and pr and token):
        warn("PR の情報が揃わないのでコメントしません")
        return 0

    try:
        if worth_saying:
            sticky_comment(render(worth_saying), repo, pr, token)
        else:
            clear_comment(repo, pr, token)
    except urllib.error.HTTPError as error:
        warn(f"コメントできませんでした（マージは止めません）: {error.code} {error.reason}")

    return 0


if __name__ == "__main__":
    sys.exit(main())
