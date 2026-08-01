//! YAML 文字列 → `T: DeserializeOwned` の共通ローダ。
//!
//! 各スキーマ型は `#[serde(deny_unknown_fields)]` を付ける前提で書く。
//! 未知フィールドを黙って無視すると、YAML のタイプミスが「設定したつもりで
//! 効いていない」という最悪の形で表面化するため（`docs/04-jp-tax.md`）。
//!
//! # 埋め込みデータと差し替え経路の両方を用意する
//!
//! - [`load_embedded`]: `kaikei-jp-data` が公開する [`EmbeddedYaml`] から読む
//!   （既定のデータ。バイナリを配布しただけで動く）
//! - [`load_from_path`][]: 任意のファイルパスから読む
//!   （ユーザーが自分の科目表・税区分マスタに差し替える経路）
//!
//! どちらも同じデシリアライズ経路（[`load_from_path`] は内部で [`load_str`]
//! を呼ぶ）を通るため、埋め込みデータとユーザー差し替え YAML の間で検証の
//! 強さに非対称が生じない。
//!
//! # I/O は許されるが、呼び出しは構築時に限る
//!
//! `kaikei-policy` と異なり `kaikei-jp` は I/O 禁止ではないため、
//! [`load_from_path`] は `std::fs` を直接使ってよい（`CLAUDE.md` §3）。
//! ただし `TaxPolicy` 等 `kaikei-policy` の trait メソッド自体は純関数を
//! 保つ必要がある（`CLAUDE.md` §3 / `DECISIONS.md` D-025）ため、これらの
//! ローダを呼ぶのは**構築時**（合成ルートの起動時）に限ること。仕訳の検証や
//! 税額計算の途中でロードし直してはいけない。

use crate::error::JpError;
use kaikei_jp_data::EmbeddedYaml;
use serde::de::DeserializeOwned;
use std::path::Path;

/// `kaikei-jp-data` の埋め込み YAML を `T` にロードする。
///
/// エラーメッセージに出す識別子は [`EmbeddedYaml::label`] から取る。
/// 呼び出し側がラベル文字列を手で書かないのは、定数とラベルの対応が
/// ずれても気づけるのがエラーメッセージの文言だけ、という状態を避けるため
/// （対応の正しさは `kaikei-jp-data` 側のテストが検証する）。
pub fn load_embedded<T>(embedded: EmbeddedYaml) -> Result<T, JpError>
where
    T: DeserializeOwned,
{
    load_str(embedded.source, embedded.label)
}

/// 任意のファイルパスから YAML を読み込み、`T` にロードする。
///
/// ユーザーが自分の勘定科目表・税区分マスタに差し替える経路。
pub fn load_from_path<T>(path: &Path) -> Result<T, JpError>
where
    T: DeserializeOwned,
{
    let label = path.display().to_string();
    let text = std::fs::read_to_string(path).map_err(|source| JpError::Io {
        path: label.clone(),
        source,
    })?;
    load_str(&text, &label)
}

/// YAML 文字列を `T` にロードする（[`load_embedded`] / [`load_from_path`] の共通部）。
///
/// `label` はエラーメッセージに含める識別子。テストから直接呼べるように
/// 公開しているが、通常は上の2つを使うこと。
pub fn load_str<T>(source: &str, label: &str) -> Result<T, JpError>
where
    T: DeserializeOwned,
{
    serde_norway::from_str(source).map_err(|source| JpError::YamlParse {
        label: label.to_string(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempFile;
    use serde::Deserialize;

    #[derive(Debug, Deserialize, PartialEq)]
    #[serde(deny_unknown_fields)]
    struct Sample {
        name: String,
        value: i32,
    }

    #[test]
    fn load_str_parses_valid_yaml() {
        let parsed: Sample = load_str("name: foo\nvalue: 1\n", "test").unwrap();
        assert_eq!(
            parsed,
            Sample {
                name: "foo".to_string(),
                value: 1,
            }
        );
    }

    #[test]
    fn load_str_unknown_field_is_yaml_parse_error_with_label_and_location() {
        let err =
            load_str::<Sample>("name: foo\nvalue: 1\nextra: true\n", "test-label").unwrap_err();
        assert!(matches!(err, JpError::YamlParse { .. }));
        let message = err.to_string();
        assert!(
            message.contains("test-label"),
            "どのファイルか分かる必要がある: {message}"
        );
        assert!(
            message.contains("line"),
            "何行目か分かる必要がある（CLAUDE.md §11）: {message}"
        );
    }

    #[test]
    fn load_str_invalid_syntax_is_yaml_parse_error() {
        // インデント崩れ。構文自体が壊れているケース。
        let err = load_str::<Sample>("name: foo\n  value: 1\n", "broken").unwrap_err();
        assert!(matches!(err, JpError::YamlParse { .. }));
    }

    #[test]
    fn load_embedded_uses_the_label_from_the_constant() {
        // 実データ（tags.yaml）を Sample として読もうとすれば必ず失敗するので、
        // ラベルが定数側から取られていることをエラーメッセージで確認できる。
        let err = load_embedded::<Sample>(kaikei_jp_data::TAGS).unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains(kaikei_jp_data::TAGS.label),
            "ラベルは EmbeddedYaml::label から取ること: {message}"
        );
    }

    #[test]
    fn load_from_path_missing_file_is_io_error() {
        let path = Path::new("does/not/exist-kaikei-jp-test.yaml");
        let err = load_from_path::<Sample>(path).unwrap_err();
        assert!(matches!(err, JpError::Io { .. }));
        assert!(
            err.to_string()
                .contains("does/not/exist-kaikei-jp-test.yaml"),
            "どのパスが読めなかったか分かる必要がある: {err}"
        );
    }

    #[test]
    fn load_from_path_reads_and_parses_existing_file() {
        let file = TempFile::with_contents("name: bar\nvalue: 2\n");
        let parsed: Sample = load_from_path(file.path()).unwrap();
        assert_eq!(
            parsed,
            Sample {
                name: "bar".to_string(),
                value: 2,
            }
        );
    }

    /// 埋め込みと差し替えで検証の強さが変わらないこと。
    #[test]
    fn load_from_path_rejects_unknown_fields_just_like_embedded() {
        let file = TempFile::with_contents("name: bar\nvalue: 2\nextra: true\n");
        let err = load_from_path::<Sample>(file.path()).unwrap_err();
        assert!(matches!(err, JpError::YamlParse { .. }));
    }
}
