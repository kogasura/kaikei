//! 銀行・カード明細の取込。
//!
//! `docs/05-csv-import.md`。
//!
//! # 取込データは仕訳ではない
//!
//! 明細の1行は**「未処理の取引記録」**であって仕訳ではない。別集約・別
//! コンテキストとして分離する（§1）。
//!
//! | コンテキスト | 語彙 |
//! |---|---|
//! | 記帳 | 仕訳・勘定科目・借方/貸方・試算表 |
//! | 取込 | 取引・入金/出金・摘要・未処理 |
//!
//! **「入金/出金」と「借方/貸方」は似ているが同じではない。** 借方は資産の増加も
//! 費用の発生も表す。直接変換せず、翻訳は `kaikei-app` の journalize が担う。
//!
//! そのため、この crate は **`kaikei-core` に依存しない**。繋ぐと2つの語彙が
//! 混ざり、設計全体が汚染される。
//!
//! # 同じ明細を2回取り込んでも重複しない
//!
//! [`external_key`] が冪等性のキーを作る。**取引後残高を含めるのが要点**で、
//! 同じ日に同じ額を同じ店で2回使った取引を区別できる（§4）。
//!
//! 残高の無い明細では、同じ内容の何件目かを添えて区別する。

pub mod profile;
pub mod reader;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// 明細の出どころ（`example_bank`・`example_card` など）。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SourceId(String);

impl SourceId {
    /// 出どころの識別子を作る。
    ///
    /// **空文字を通さない。** 出どころが空だと、別々の口座の明細が同じ
    /// 冪等性キーで衝突しうる。
    pub fn parse(text: &str) -> Result<Self, ImportError> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err(ImportError::InvalidValue {
                reason: "明細の出どころ（source）が空です".to_string(),
            });
        }
        Ok(SourceId(trimmed.to_string()))
    }

    /// 文字列表現。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// 入金か出金か。
///
/// **借方/貸方ではない。** どちらの勘定科目に立てるかは翻訳側が決める。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Direction {
    /// 入金。
    In,
    /// 出金。
    Out,
}

impl Direction {
    /// 冪等性キーに混ぜるための短い表現。
    fn as_key(&self) -> &'static str {
        match self {
            Direction::In => "in",
            Direction::Out => "out",
        }
    }
}

/// 取り込んだ明細1行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedTransaction {
    /// 出どころ。
    pub source: SourceId,
    /// 冪等性のキー（[`external_key`] が作る）。
    pub external_key: String,
    /// 取引日。
    pub occurred_on: chrono::NaiveDate,
    /// 金額。**常に正**で、向きは [`Direction`] が表す。
    pub amount_minor: i64,
    /// 通貨。
    pub currency: String,
    /// 入金か出金か。
    pub direction: Direction,
    /// 明細の摘要（`ｶ)ｻﾝﾌﾟﾙ ｼﾖｳｼﾞ` のような生の文字列）。
    pub raw_description: String,
    /// 取引後残高。**冪等性キーに効く**ので、あれば必ず入れる。
    pub balance_after: Option<i64>,
    /// 元の CSV 行そのもの。
    ///
    /// **捨てない。** 解釈を間違えたと後で分かったとき、元が無ければ直せない。
    pub raw_row: serde_json::Value,
}

/// 取込で起きる失敗。
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ImportError {
    /// 値が不正。
    #[error("取り込めない値です: {reason}")]
    InvalidValue {
        /// 理由。
        reason: String,
    },
    /// 文字コードを決められない。
    #[error("{reason}")]
    Undecodable {
        /// 理由。
        reason: String,
    },
}

/// 冪等性のキーを作る。
///
/// `docs/05-csv-import.md` §4。
///
/// # 取引後残高を含める
///
/// 同じ日に同じ額を同じ店で2回使うことは普通にある。日付・金額・向き・摘要
/// だけでは区別できず、**2件目を「重複」として捨ててしまう**。取引後残高が
/// あれば必ず違う値になるので、これを含める。
///
/// # 残高が無いとき
///
/// カード明細には残高が無いことが多い。その場合は `occurrence`（同じ内容の
/// 何件目か。CSV 内の出現順）で区別する。**呼び出し側が数える**——この関数は
/// 1行だけを見るので、何件目かを知らない。
pub fn external_key(
    occurred_on: chrono::NaiveDate,
    amount_minor: i64,
    direction: Direction,
    description: &str,
    balance_after: Option<i64>,
    occurrence: u32,
) -> String {
    let mut hasher = Sha256::new();
    // 区切りを入れる。**入れないと「12|3」と「1|23」が同じキーになる。**
    hasher.update(occurred_on.to_string().as_bytes());
    hasher.update(b"\x1f");
    hasher.update(amount_minor.to_string().as_bytes());
    hasher.update(b"\x1f");
    hasher.update(direction.as_key().as_bytes());
    hasher.update(b"\x1f");
    hasher.update(description.as_bytes());
    hasher.update(b"\x1f");
    match balance_after {
        Some(balance) => hasher.update(balance.to_string().as_bytes()),
        // 残高が無いことと「残高が0」を混ぜない。
        None => hasher.update(b"(none)"),
    }
    hasher.update(b"\x1f");
    hasher.update(occurrence.to_string().as_bytes());

    let digest = hasher.finalize();
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push(char::from_digit((byte >> 4) as u32, 16).expect("0..=15 は16進の桁"));
        out.push(char::from_digit((byte & 0x0f) as u32, 16).expect("0..=15 は16進の桁"));
    }
    out
}

/// CSV のバイト列を文字列にする。
///
/// `docs/05-csv-import.md`。邦銀の明細は **Shift-JIS が多い**。
///
/// # 文字コードを推測で決めない
///
/// UTF-8 として読めればそれを使い、読めなければ Shift-JIS を試す。
/// **どちらでも読めなければエラーにする**——置換文字だらけの文字列を返すと、
/// 摘要が壊れたまま帳簿に入る（実際に別の経路で踏んだ事故である）。
///
/// UTF-8 の BOM は取り除く（残すと最初の列名が一致しない）。
pub fn decode_csv(bytes: &[u8]) -> Result<String, ImportError> {
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);

    if let Ok(text) = std::str::from_utf8(bytes) {
        return Ok(text.to_string());
    }

    let (text, _, had_errors) = encoding_rs::SHIFT_JIS.decode(bytes);
    if had_errors {
        return Err(ImportError::Undecodable {
            reason: "この明細は UTF-8 でも Shift-JIS でも読めません。\
                     文字コードを確かめてください（壊れた文字のまま取り込むことはしません）"
                .to_string(),
        });
    }
    Ok(text.into_owned())
}

/// 取込の結果。
///
/// **部分成功を許す**（§4）。1行のパースに失敗しても全体を止めない。
/// 失敗した行は理由付きで返し、利用者が判断できるようにする。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportResult {
    /// 取り込んだ件数。
    pub inserted: usize,
    /// 既にあったので飛ばした件数。
    pub skipped_duplicate: usize,
    /// 取り込めなかった行。
    pub errors: Vec<RowError>,
}

/// 取り込めなかった行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowError {
    /// CSV の行番号（1 始まり。見出し行を含む）。
    pub line: usize,
    /// 理由。
    pub reason: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(year: i32, month: u32, day: u32) -> chrono::NaiveDate {
        chrono::NaiveDate::from_ymd_opt(year, month, day).unwrap()
    }

    fn key(description: &str, balance: Option<i64>, occurrence: u32) -> String {
        external_key(
            date(2026, 6, 15),
            1_000,
            Direction::Out,
            description,
            balance,
            occurrence,
        )
    }

    // IM-1: 同じ内容からは同じキーが出る（2回取り込んでも重複しない）。
    #[test]
    fn the_same_row_always_produces_the_same_key() {
        assert_eq!(key("ｺﾝﾋﾞﾆ", Some(50_000), 0), key("ｺﾝﾋﾞﾆ", Some(50_000), 0));
    }

    // IM-2: **本命。** 同日・同額・同摘要でも、取引後残高が違えば別の取引。
    //
    //       同じ日に同じ店で2回買うことは普通にある。ここを区別できないと、
    //       2件目を「重複」として捨ててしまう。
    #[test]
    fn two_identical_looking_transactions_are_told_apart_by_the_balance() {
        let first = key("ｺﾝﾋﾞﾆ", Some(50_000), 0);
        let second = key("ｺﾝﾋﾞﾆ", Some(49_000), 0);

        assert_ne!(first, second, "取引後残高で区別できていない");
    }

    // IM-3: 残高が無い明細（カードなど）は出現順で区別する。
    #[test]
    fn without_a_balance_the_occurrence_tells_them_apart() {
        assert_ne!(key("ｺﾝﾋﾞﾆ", None, 0), key("ｺﾝﾋﾞﾆ", None, 1));
    }

    // IM-4: 「残高が無い」と「残高が0」を混ぜない。
    #[test]
    fn a_missing_balance_is_not_the_same_as_a_zero_balance() {
        assert_ne!(key("ｺﾝﾋﾞﾆ", None, 0), key("ｺﾝﾋﾞﾆ", Some(0), 0));
    }

    // IM-5: 項目の区切りが効く。
    //
    //       区切りが無いと、隣り合う項目の切れ目が違うだけの別の行が
    //       同じキーになる。
    #[test]
    fn adjacent_fields_cannot_run_together() {
        let a = external_key(date(2026, 6, 15), 12, Direction::Out, "3", None, 0);
        let b = external_key(date(2026, 6, 15), 1, Direction::Out, "23", None, 0);

        assert_ne!(a, b);
    }

    // IM-6: 入金と出金は別の取引。
    #[test]
    fn a_deposit_and_a_withdrawal_are_different() {
        let deposit = external_key(date(2026, 6, 15), 1_000, Direction::In, "x", None, 0);
        let withdrawal = external_key(date(2026, 6, 15), 1_000, Direction::Out, "x", None, 0);

        assert_ne!(deposit, withdrawal);
    }

    // IM-7: **本命。** Shift-JIS の明細が文字化けせずに読める。
    #[test]
    fn a_shift_jis_statement_is_decoded() {
        let (bytes, _, _) = encoding_rs::SHIFT_JIS.encode("日付,摘要,金額\n2026-06-15,ｺﾝﾋﾞﾆ,1000\n");

        let text = decode_csv(&bytes).unwrap();

        assert!(text.contains("摘要"), "{text}");
        assert!(text.contains("ｺﾝﾋﾞﾆ"), "{text}");
        assert!(!text.contains('\u{fffd}'), "置換文字が混じっている: {text}");
    }

    // IM-8: UTF-8 の明細もそのまま読める。BOM は取り除く。
    #[test]
    fn a_utf8_statement_is_decoded_and_its_bom_removed() {
        let mut bytes = b"\xef\xbb\xbf".to_vec();
        bytes.extend_from_slice("日付,摘要\n2026-06-15,コンビニ\n".as_bytes());

        let text = decode_csv(&bytes).unwrap();

        assert!(text.starts_with("日付"), "BOM が残っている: {text:?}");
    }

    // IM-9: **本命。** どちらでも読めなければエラーにする。
    //
    //       置換文字だらけの文字列を返すと、摘要が壊れたまま帳簿に入る。
    #[test]
    fn an_undecodable_statement_is_rejected_not_mangled() {
        // UTF-8 としても Shift-JIS としても妥当でないバイト列。
        let bytes = vec![0x80, 0xfd, 0xfe, 0xff, 0x81, 0x20];

        let error = decode_csv(&bytes).expect_err("読めないなら拒否すること");

        assert!(
            format!("{error}").contains("文字コードを確かめて"),
            "{error}"
        );
    }

    // IM-10: 出どころが空なら拒否する。
    //
    //        空だと、別々の口座の明細が同じキーで衝突しうる。
    #[test]
    fn an_empty_source_is_rejected() {
        assert!(SourceId::parse("").is_err());
        assert!(SourceId::parse("   ").is_err());
        assert_eq!(
            SourceId::parse(" example_card ").unwrap().as_str(),
            "example_card"
        );
    }

    // IM-11: 部分成功を表せる（1行の失敗で全体を止めない）。
    #[test]
    fn the_result_can_express_a_partial_success() {
        let result = ImportResult {
            inserted: 8,
            skipped_duplicate: 1,
            errors: vec![RowError {
                line: 5,
                reason: "金額を数として読めません".to_string(),
            }],
        };

        assert_eq!(result.inserted, 8);
        assert_eq!(result.errors[0].line, 5);
    }
}
