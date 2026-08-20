#!/usr/bin/env python3
"""実データの混入を、**値ではなく形**で検査する（CLAUDE.md §14 / §15）。

# なぜ「値」で検査しないのか

`105,600` が架空値か実額かは、値そのものからは判定できない。だから
「実データを検出する」検査は原理的に作れない。**除去済みの値を並べた
denylist も作らない**——それを public リポジトリに置くのは本末転倒である
（ハッシュ化しても、社名や金額の探索空間は総当たりで戻せる）。

# 代わりに何を見るか

これまでに見つかった漏洩は、例外なく次のどちらかの形をしていた。

1. 形式で機密と分かる文字列（鍵・トークン・メール・住所・ローカルパス）
2. **「実帳簿では〜」と書いて、その場に具体的な数を添える**書き方

1 は値の形で判定できる。2 は値ではなく**書き方**なので、まだ知らない実額に
も効く。CLAUDE.md §15 は「事業者は名前を付けず検証帳簿と呼ぶ」と決めている
ので、`実帳簿` の語が残っているところは「本物の話をしている」という宣言に
なる。そこに数が並んでいたら、それは実額である可能性が高い。

# 逃げ道

誤検知は必ず出る。既存の `ci-allow: append-only-probe`（architecture.yml）と
同じ流儀で、行末に印を置けば個別に外せる。**印を付ける = 人が確認した**
という意味なので、黙って握りつぶすのとは違う。

    ci-allow: secret-shaped        … 1 の検査を外す
    ci-allow: real-ledger-mention  … 2 の検査を外す

# 使い方

    python3 .github/scripts/no_real_data.py            # 追跡ファイル全体を検査
    python3 .github/scripts/no_real_data.py --diff origin/main
                                                       # 加えて、差分が持ち込んだ
                                                       # 非まるめの数値を一覧する
"""

from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys

# 検査対象にする拡張子。バイナリと Cargo.lock は見ない。
TEXT_SUFFIXES = {
    ".rs", ".md", ".yaml", ".yml", ".sql", ".toml", ".txt", ".example",
    ".json",
}

# 拡張子を持たないが検査したいファイル。**`splitext(".gitignore")` が返すのは
# `(".gitignore", "")` で、拡張子は空文字列である。** 拡張子の集合に名前を
# 並べても素通りするので、名前の側で拾う。ローカルパスが混ざりやすい場所。
TEXT_NAMES = {".gitignore", ".gitattributes"}

# この検査スクリプト自身は、検査したいパターンを本文に持つので対象外。
SELF = ".github/scripts/no_real_data.py"

# --- 1. 形式で機密と分かるもの ------------------------------------------

SECRET_SHAPED: list[tuple[str, str]] = [
    (r"gh[pousr]_[A-Za-z0-9]{16,}", "GitHub のトークン"),
    (r"github_pat_[A-Za-z0-9_]{20,}", "GitHub の fine-grained トークン"),
    (r"sk-[A-Za-z0-9_-]{16,}", "API キー（sk- 形式）"),
    (r"A(?:KIA|SIA)[0-9A-Z]{16}", "AWS のアクセスキー"),
    (r"AIza[0-9A-Za-z_-]{20,}", "Google API キー"),
    (r"xox[baprs]-[A-Za-z0-9-]{10,}", "Slack のトークン"),
    (r"-----BEGIN [A-Z ]*PRIVATE KEY-----", "秘密鍵"),
    (r"eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.", "JWT"),
    (r"[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}",
     "メールアドレス"),
    (r"(?<![0-9A-Fa-f-])0\d{1,3}-\d{1,4}-\d{3,4}(?![0-9A-Fa-f-])", "電話番号"),
    (r"〒\s*\d{3}-?\d{4}", "郵便番号"),
    (r"(東京都|大阪府|京都府|北海道)[一-龥]{2,}[区市郡]", "住所"),
    (r"[一-龥]{2,3}県[一-龥]{2,4}[区市郡]", "住所"),
    (r"/home/[a-z][a-z0-9_-]*", "ローカルパス"),
    (r"/Users/[A-Za-z][A-Za-z0-9_-]*", "ローカルパス"),
    (r"C:\\Users\\[A-Za-z0-9]", "ローカルパス"),
    (r"(個人番号|マイナンバー)[^\n]{0,20}\d{12}", "マイナンバーらしき12桁"),
]

# **意図して置いてあるダミー**。これらは伏字の検査そのものに要る。
#   - db.internal        … エラーが接続文字列を伏せることの検査（error.rs）
#   - noreply@…          … §12 の AI 帰属表示が「禁止語」として書かれている
SECRET_ALLOW = (
    "db.internal",
    "noreply@anthropic.com",
    "noreply@github.com",
)

# --- 2. 「実帳簿」と具体的な数の同居 ------------------------------------

REAL_LEDGER = re.compile(r"実帳簿|実際の帳簿|本番の帳簿|実運用の帳簿")

# 3桁以上の数。識別子の一部（D-101、E0432）と桁区切りの途中は数えない。
#
# **後読みを `\w` で書いてはいけない。** Python の `\w` は Unicode の単語文字
# なので、かな・漢字にもマッチする。`実帳簿では105,600円` の `1` の直前は `は`
# であり、`\w` だと否定後読みがそこで成立せず——`0` の前は `1`、というように
# 先も潰れるので——**その行から1件も拾えなくなる**。日本語の地の文に埋まった
# 金額こそが拾いたいものなので、ASCII の英数字と `_` だけに限る。
BIG_NUMBER = re.compile(
    r"(?<![-0-9A-Za-z_.,])\d{1,3}(?:,\d{3})+(?![\d,])"
    r"|(?<![-0-9A-Za-z_.,])\d{3,}(?![\d,])"
)
# 「9件」のような小さな数も、規模を示すなら拾う。後読みの理由は上と同じ。
COUNTED = re.compile(r"(?<![-0-9A-Za-z_])(\d+)\s*(件|行|明細|取引|社|名)")

# 年は数えない（「実帳簿の2026年」は規模を示さない）。
def _is_year(text: str) -> bool:
    digits = text.replace(",", "")
    return digits.isdigit() and 1900 <= int(digits) <= 2100


# 「実帳簿」の行の前後、何行までを同じ文脈と見るか。
# doc コメントは3行程度で1つの文になることが多い。**前も1行見る**——表や箇条書き
# の下に「実帳簿ではこうだった」と注記を置く書き方があり、数はその上にある。
CONTEXT_BEFORE = 1
CONTEXT_AFTER = 3


def tracked_files() -> list[str]:
    out = subprocess.check_output(["git", "ls-files"], text=True)
    return [f for f in out.split("\n") if f]


def is_text(path: str) -> bool:
    return (os.path.splitext(path)[1] in TEXT_SUFFIXES
            or os.path.basename(path) in TEXT_NAMES)


def read_lines(path: str) -> list[str] | None:
    if not is_text(path):
        return None
    try:
        with open(path, encoding="utf-8") as handle:
            return handle.read().splitlines()
    except (UnicodeDecodeError, IsADirectoryError, FileNotFoundError):
        return None


def check_secret_shaped(path: str, lines: list[str]) -> list[str]:
    problems = []
    for no, line in enumerate(lines, 1):
        if "ci-allow: secret-shaped" in line:
            continue
        for pattern, label in SECRET_SHAPED:
            for found in re.finditer(pattern, line):
                hit = found.group(0)
                if any(allowed in hit for allowed in SECRET_ALLOW):
                    continue
                problems.append(f"{path}:{no}: {label}らしき文字列: {hit}")
    return problems


def check_real_ledger(path: str, lines: list[str]) -> list[str]:
    problems = []
    for index, line in enumerate(lines):
        if not REAL_LEDGER.search(line):
            continue
        window = lines[max(0, index - CONTEXT_BEFORE):index + CONTEXT_AFTER]
        if any("ci-allow: real-ledger-mention" in text for text in window):
            continue
        for text in window:
            numbers = [m.group(0) for m in BIG_NUMBER.finditer(text)]
            numbers += [
                m.group(0) for m in COUNTED.finditer(text) if int(m.group(1)) >= 2
            ]
            numbers = [n for n in numbers if not _is_year(n)]
            if numbers:
                problems.append(
                    f"{path}:{index + 1}: 「実帳簿」と具体的な数 "
                    f"{'、'.join(sorted(set(numbers))[:4])} が同じ文脈にあります"
                )
                break
    return problems


def _interesting_numbers(text: str) -> list[str]:
    """非まるめの数値だけを拾う。丸い数・年・ゾロ目は架空値として自然なので除く。"""
    out = []
    for found in BIG_NUMBER.finditer(text):
        raw = found.group(0)
        value = raw.replace(",", "")
        if not value.isdigit() or _is_year(raw):
            continue
        if int(value) % 1000 == 0 or len(set(value)) == 1:
            continue
        out.append(raw)
    return out


def added_numbers(base: str) -> list[str]:
    """差分が持ち込んだ非まるめの数値を一覧する（落とさない・知らせるだけ）。

    **削除行と相殺する。** 語の言い換えのように、同じ数を含む行を書き換えた
    だけの差分で一覧が埋まると読まれなくなる。ここで出したいのは
    「この PR で新しく現れた数」である。
    """
    try:
        diff = subprocess.check_output(
            ["git", "diff", "--unified=0", f"{base}...HEAD"], text=True,
            stderr=subprocess.DEVNULL,
        )
    except subprocess.CalledProcessError:
        return []

    path = ""
    added: dict[str, list[str]] = {}
    removed: set[str] = set()
    for line in diff.splitlines():
        if line.startswith("+++ b/"):
            path = line[6:]
            continue
        if line.startswith("--- ") or line.startswith("+++ "):
            continue
        if not is_text(path):
            continue
        if line.startswith("+"):
            for raw in _interesting_numbers(line[1:]):
                added.setdefault(raw, []).append(path)
        elif line.startswith("-"):
            removed.update(_interesting_numbers(line[1:]))

    return [f"{raw}（{sorted(set(paths))[0]} ほか）" if len(set(paths)) > 1
            else f"{raw}（{paths[0]}）"
            for raw, paths in sorted(added.items()) if raw not in removed]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--diff", metavar="BASE",
                        help="この参照との差分が持ち込んだ数値を一覧する")
    args = parser.parse_args()

    secrets: list[str] = []
    ledger: list[str] = []
    for path in tracked_files():
        if path == SELF:
            continue
        lines = read_lines(path)
        if lines is None:
            continue
        secrets += check_secret_shaped(path, lines)
        ledger += check_real_ledger(path, lines)

    failed = False

    if secrets:
        failed = True
        print("::error::形式で機密と分かる文字列が見つかりました")
        for problem in secrets:
            print(f"  {problem}")
        print("  意図して置いたダミーなら、その行に `ci-allow: secret-shaped` を書くこと。")
    else:
        print("OK: 形式で機密と分かる文字列は見つかりませんでした")

    if ledger:
        failed = True
        print("::error::「実帳簿」の語と具体的な数が同じ文脈にあります")
        for problem in ledger:
            print(f"  {problem}")
        print()
        print("  CLAUDE.md §15: 事業者は名前を付けず「検証帳簿」と呼び、金額は桁と")
        print("  演算関係だけを似せた架空値にする。実額でないなら「検証帳簿」に")
        print("  言い換えること。実額でも数でもないと確認したなら、その行に")
        print("  `ci-allow: real-ledger-mention` を書くこと。")
    else:
        print("OK: 「実帳簿」と具体的な数の同居はありません")

    if args.diff:
        numbers = added_numbers(args.diff)
        summary = os.environ.get("GITHUB_STEP_SUMMARY")
        lines = ["## この PR が持ち込んだ非まるめの数値", ""]
        if numbers:
            lines.append(
                f"{len(numbers)} 件。**実帳簿から取った値が混ざっていないか**"
                "確かめてください（CLAUDE.md §14）。"
            )
            lines.append("")
            lines += [f"- `{item}`" for item in numbers]
        else:
            lines.append("なし。")
        text = "\n".join(lines)
        print()
        print(text)
        if summary:
            with open(summary, "a", encoding="utf-8") as handle:
                handle.write(text + "\n")

    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
