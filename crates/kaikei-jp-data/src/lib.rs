//! 年度別 YAML（税区分／科目／タグスキーマ）を `&'static str` として公開する。
//!
//! # このcrateはデータだけを持つ
//!
//! **ロジックを一切持たない**（`[dependencies]` はゼロ。CI が検査する）。
//! YAML の解釈（デシリアライズ・スキーマ検証）は `kaikei-jp` が行う
//! （`docs/04-jp-tax.md` §1「税率も控除割合もコードに書かない」）。ここでは
//! `include_str!` で YAML をバイナリへ埋め込むだけで、実行時のファイルパス
//! 依存を作らない（バイナリを配布しただけで動く状態を保つ。`ARCHITECTURE.md` §7）。
//!
//! # 自分の YAML に差し替える
//!
//! ここで公開する定数はビルド時に埋め込まれた既定データであり、実行時に
//! 差し替えることはできない。ユーザーが自分の勘定科目表・税区分マスタに
//! 差し替えたい場合は、`kaikei-jp` 側が提供する「任意のパスから読む」
//! ローダ（`kaikei_jp::yaml::load_from_path`）を使い、自分の YAML
//! ファイルへのパスを渡すこと。
//!
//! # 税区分マスタは「年度」ではなく「適用期間」で引く
//!
//! 消費税区分マスタは複数の版が並ぶ。**選択は取引日で行う**（記帳日ではない。
//! `CLAUDE.md` §7）。ただし選択ロジック自体はこの crate の責務ではなく、
//! `kaikei-jp` の `JpTaxPolicy` が構築時に全件読み、各 YAML の
//! `applies_from` / `applies_to` を見て決める（`DECISIONS.md` D-025 / D-050）。
//!
//! このcrateは [`TAX_CATEGORY_SOURCES`] で**埋め込み済みの全マスタを列挙する**
//! だけを行う。「暦年 → マスタ1件」という引き方は提供しない。日本の消費税率
//! 改正は年度途中に起きるのが通例で（2019年10月の軽減税率導入など）、
//! 暦年会計の個人事業主にとっては**1つの暦年に2つのマスタが適用される**
//! 期間が実際に生じるため、暦年をキーにすると表現できなくなる。

#![forbid(unsafe_code)]
#![warn(missing_docs)]

/// 埋め込み YAML 1件。
///
/// `label` はエラーメッセージに出す識別子（リポジトリ内のパス）。定数と
/// ラベルを対にして持つことで、呼び出し側がラベル文字列を手で書く必要が
/// 無くなる（書き間違えても気づけるのはエラーメッセージの文言だけ、という
/// 状態を避ける）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmbeddedYaml {
    /// エラーメッセージ用の識別子（リポジトリ内のパス）。
    pub label: &'static str,
    /// YAML の中身。
    pub source: &'static str,
}

/// タグスキーマ（`tags.yaml`）。
///
/// 新しいタグキーが必要になったらこのファイルに登録する。`kaikei_core`
/// の `JournalEntry::new` が未登録キーを拒否する（`CLAUDE.md` §4）。
pub const TAGS: EmbeddedYaml = EmbeddedYaml {
    label: "kaikei-jp-data/tags.yaml",
    source: include_str!("../tags.yaml"),
};

/// 勘定科目テンプレート（個人事業主・青色申告向け）。
///
/// ユーザーはこれを複製して自由に編集できる（`docs/04-jp-tax.md` §5）。
pub const CHART_SOLE_PROPRIETOR: EmbeddedYaml = EmbeddedYaml {
    label: "kaikei-jp-data/chart/sole_proprietor.yaml",
    source: include_str!("../chart/sole_proprietor.yaml"),
};

/// 埋め込み済みの消費税区分マスタ**全件**。
///
/// 適用期間（各 YAML の `applies_from` / `applies_to`）による選択は
/// `kaikei-jp` 側が行う。このスライスは「どのマスタが同梱されているか」の
/// 唯一の情報源であり、**マスタを追加したらここに1行足すだけ**でよい
/// （`kaikei-jp` 側に年度の一覧を別途持たせない。手で維持する一覧を2箇所に
/// 増やさないため。`DECISIONS.md` D-047 / D-050）。
///
/// 並び順に意味は持たせない（適用期間で選ぶため）。
///
/// ファイル名は**施行日**である（暦年ではない）。消費税の改正は暦年の途中に
/// 施行されるため、1つの暦年に2つのマスタが並ぶ年がある（`DECISIONS.md` D-092）。
pub const TAX_CATEGORY_SOURCES: &[EmbeddedYaml] = &[
    EmbeddedYaml {
        label: "kaikei-jp-data/tax/jp/2026-01-01.yaml",
        source: include_str!("../tax/jp/2026-01-01.yaml"),
    },
    EmbeddedYaml {
        label: "kaikei-jp-data/tax/jp/2026-10-01.yaml",
        source: include_str!("../tax/jp/2026-10-01.yaml"),
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_sources_are_not_empty() {
        assert!(!TAGS.source.is_empty(), "tags.yaml が空です");
        assert!(
            !CHART_SOLE_PROPRIETOR.source.is_empty(),
            "sole_proprietor.yaml が空です"
        );
        for entry in TAX_CATEGORY_SOURCES {
            assert!(!entry.source.is_empty(), "{} が空です", entry.label);
        }
    }

    #[test]
    fn tax_category_sources_is_not_empty() {
        assert!(
            !TAX_CATEGORY_SOURCES.is_empty(),
            "消費税区分マスタが1件も埋め込まれていません。\
             kaikei-jp 側は「最低1件は存在する」前提で組み立てます"
        );
    }

    /// ラベルは重複しない（エラーメッセージでどのファイルか特定できなくなるため）。
    #[test]
    fn labels_are_unique() {
        let mut labels: Vec<&str> = TAX_CATEGORY_SOURCES.iter().map(|e| e.label).collect();
        labels.push(TAGS.label);
        labels.push(CHART_SOLE_PROPRIETOR.label);
        let mut sorted = labels.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            labels.len(),
            "ラベルが重複しています: {labels:?}"
        );
    }

    /// ラベルは実在のパスを指す（`include_str!` のパスとラベルの食い違いを防ぐ）。
    ///
    /// `include_str!` はコンパイル時にパスを解決するが、`label` はただの文字列
    /// なので、片方だけ直すと「エラーメッセージが別のファイルを指す」という
    /// 誤診を生む。ここで両者が同じファイルを指していることを確認する。
    #[test]
    fn labels_point_at_files_whose_contents_match() {
        // CARGO_MANIFEST_DIR は crates/kaikei-jp-data。ラベルはその1つ上からの相対パス。
        let workspace_crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates/kaikei-jp-data の親ディレクトリは常に存在する")
            .to_path_buf();

        let mut all: Vec<EmbeddedYaml> = TAX_CATEGORY_SOURCES.to_vec();
        all.push(TAGS);
        all.push(CHART_SOLE_PROPRIETOR);

        for entry in all {
            let path = workspace_crates.join(entry.label);
            let on_disk = std::fs::read_to_string(&path).unwrap_or_else(|e| {
                panic!(
                    "label \"{}\" が指すファイルを読めません（{}）: {e}\n\
                     label と include_str! のパスが食い違っている可能性があります",
                    entry.label,
                    path.display()
                )
            });
            // 改行コードの差（CRLF/LF）は本質ではないので正規化して比較する。
            assert_eq!(
                on_disk.replace("\r\n", "\n"),
                entry.source.replace("\r\n", "\n"),
                "label \"{}\" が指すファイルの中身が、埋め込まれた内容と一致しません。\
                 label と include_str! が別のファイルを指しています",
                entry.label
            );
        }
    }
}
