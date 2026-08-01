//! 適格請求書発行事業者登録番号（`T` + 13桁）。
//!
//! `docs/04-jp-tax.md` §6・`docs/08-compliance.md` §6 を参照。

use crate::error::JpError;

/// 適格請求書発行事業者登録番号（`T` + 13桁。法人番号または個人事業主用の13桁）。
///
/// # この型が保証すること・保証しないこと
///
/// - **保証する**: 文字列が `T` + 数字13桁という**形式**として正しく、
///   先頭1桁（検査用数字）が残り12桁（基礎番号）から計算したチェックデジットと
///   一致すること
/// - **保証しない**: その番号が国税庁に実在登録されていること、登録されている
///   事業者が適格請求書発行事業者として有効であること。実在確認・適格性の判定は
///   国税庁の公表サイト／API の領域であり、この crate では扱わない
///   （`docs/04-jp-tax.md` §6「注意」・`docs/08-compliance.md` §6「実在確認はしない」）
///
/// 「登録番号として正しい形式である」ことと「その事業者が適格請求書発行事業者
/// である」ことは別の話である。適格事業者かどうかはユーザーが
/// `Counterparty::is_qualified_invoice_issuer` に記録するものであり
/// （`kaikei-policy::counterparty`、`DECISIONS.md` D-028）、この型の責務ではない。
///
/// # トリムしない
///
/// 前後に空白を含む入力はトリムせずエラーにする。トリムして受理すると、
/// 見た目は正しく見えても実際には空白混じりの値が保存されうるため、
/// ユーザー入力の境界（フォーム・CSV取込等）で気づけるよう厳格に弾く。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct InvoiceRegistrationNo(String);

impl InvoiceRegistrationNo {
    /// 文字列を登録番号としてパースする。
    ///
    /// 検証内容（`docs/04-jp-tax.md` §6）:
    /// 1. 先頭が `'T'`（大文字。小文字 `'t'` や他の文字は不可）
    /// 2. 続く13桁が半角数字のみ（全角数字・ハイフン・空白は不可）
    /// 3. 先頭1桁（検査用数字）が、残り12桁（基礎番号）から計算した
    ///    チェックデジットと一致する
    pub fn parse(s: &str) -> Result<Self, JpError> {
        let Some(rest) = s.strip_prefix('T') else {
            return Err(JpError::InvoiceRegNoMissingPrefix {
                input: s.to_string(),
            });
        };

        let len = rest.chars().count();
        if len != 13 {
            return Err(JpError::InvoiceRegNoWrongLength {
                input: s.to_string(),
                actual_len: len,
            });
        }

        if !rest.chars().all(|c| c.is_ascii_digit()) {
            return Err(JpError::InvoiceRegNoNonDigit {
                input: s.to_string(),
            });
        }

        // 上の桁数・数字チェックを通過したため、`rest` は13桁のASCII数字のみで
        // 構成されている。1バイト文字のみなのでバイト添字でのスライスも
        // 文字境界と一致し安全。
        let written_check_digit = (rest.as_bytes()[0] - b'0') as u32;
        let base = &rest[1..]; // 12桁の基礎番号

        let expected_check_digit = compute_check_digit(base);
        if written_check_digit != expected_check_digit {
            return Err(JpError::InvoiceRegNoCheckDigit {
                input: s.to_string(),
                expected: expected_check_digit,
                actual: written_check_digit,
            });
        }

        Ok(InvoiceRegistrationNo(s.to_string()))
    }

    /// 登録番号の文字列表現を返す（例: `"T1234567890123"`）。
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// `T` を除いた13桁部分を返す（例: `"1234567890123"`）。
    pub fn corporate_number(&self) -> &str {
        &self.0[1..]
    }
}

/// 法人番号のチェックデジットを計算する（`docs/04-jp-tax.md` §6）。
///
/// ```text
/// 検査用数字 = 9 - (Σ(P_n × Q_n) mod 9)
///   P_n : 基礎番号の1桁目（最下位）から12桁目までの数字
///   Q_n : n が奇数のとき 1、偶数のとき 2
/// ```
///
/// `base` は12桁のASCII数字のみで構成されていることを呼び出し側
/// （[`InvoiceRegistrationNo::parse`]）が検証済みという前提で呼ばれる。
fn compute_check_digit(base: &str) -> u32 {
    // digits[0] が基礎番号の最上位桁（B12）、digits[11] が最下位桁（B1）。
    let digits: Vec<u32> = base.bytes().map(|b| (b - b'0') as u32).collect();
    debug_assert_eq!(
        digits.len(),
        12,
        "baseは12桁であることを呼び出し側で検証済み"
    );

    // P_n（最下位から n 桁目の数字）は digits[12 - n] に対応する
    // （n=1 → digits[11]（最下位）、n=12 → digits[0]（最上位））。
    let sum: u32 = (1..=12u32)
        .map(|n| {
            let p_n = digits[(12 - n) as usize];
            let q_n = if n % 2 == 1 { 1 } else { 2 };
            p_n * q_n
        })
        .sum();

    9 - (sum % 9)
}

#[cfg(test)]
mod tests {
    use super::*;

    // 手計算した例（実装コメント・PRの手計算過程と同じ値）。
    //
    // 基礎番号: 123456789012
    // 最下位から P_1..P_12 = 2,1,0,9,8,7,6,5,4,3,2,1
    // Q_n（nが奇数→1、偶数→2）: 1,2,1,2,1,2,1,2,1,2,1,2
    // 積 P_n * Q_n : 2,2,0,18,8,14,6,10,4,6,2,2 → 合計 74
    // 74 mod 9 = 2 → 検査用数字 = 9 - 2 = 7
    // よって "T" + "7" + "123456789012" = "T7123456789012" が有効な登録番号。
    const VALID: &str = "T7123456789012";

    #[test]
    fn parse_valid_succeeds() {
        let parsed = InvoiceRegistrationNo::parse(VALID).unwrap();
        assert_eq!(parsed.as_str(), VALID);
    }

    #[test]
    fn parse_missing_t_prefix_is_error() {
        let err = InvoiceRegistrationNo::parse("1234567890123").unwrap_err();
        assert!(matches!(err, JpError::InvoiceRegNoMissingPrefix { .. }));
    }

    #[test]
    fn parse_lowercase_t_is_error() {
        let err = InvoiceRegistrationNo::parse("t7123456789012").unwrap_err();
        assert!(matches!(err, JpError::InvoiceRegNoMissingPrefix { .. }));
    }

    #[test]
    fn parse_different_leading_character_is_error() {
        let err = InvoiceRegistrationNo::parse("X7123456789012").unwrap_err();
        assert!(matches!(err, JpError::InvoiceRegNoMissingPrefix { .. }));
    }

    #[test]
    fn parse_12_digits_is_wrong_length_error() {
        // 基礎番号の桁が1つ欠けている（12文字しかない）。
        let err = InvoiceRegistrationNo::parse("T712345678901").unwrap_err();
        assert!(matches!(
            err,
            JpError::InvoiceRegNoWrongLength { actual_len: 12, .. }
        ));
    }

    #[test]
    fn parse_14_digits_is_wrong_length_error() {
        let err = InvoiceRegistrationNo::parse("T71234567890123").unwrap_err();
        assert!(matches!(
            err,
            JpError::InvoiceRegNoWrongLength { actual_len: 14, .. }
        ));
    }

    #[test]
    fn parse_fullwidth_digits_is_non_digit_error() {
        // 全角数字13桁（見た目は"T"+13桁だが半角数字ではない）。
        let err = InvoiceRegistrationNo::parse("T１２３４５６７８９０１２３").unwrap_err();
        assert!(matches!(err, JpError::InvoiceRegNoNonDigit { .. }));
    }

    #[test]
    fn parse_hyphen_mixed_in_is_wrong_length_error() {
        let err = InvoiceRegistrationNo::parse("T712345-6789012").unwrap_err();
        // ハイフンが混ざると文字数も13から変わるため、桁数エラーになりうる。
        // ここでは全体の長さが14になるケースを確認する。
        assert!(matches!(
            err,
            JpError::InvoiceRegNoWrongLength { actual_len: 14, .. }
        ));
    }

    #[test]
    fn parse_hyphen_same_length_is_non_digit_error() {
        // 数字を1桁潰してハイフンに置き換え、長さは13のまま数字以外を混入させる。
        let err = InvoiceRegistrationNo::parse("T71234567890-2").unwrap_err();
        assert!(matches!(err, JpError::InvoiceRegNoNonDigit { .. }));
    }

    #[test]
    fn parse_space_mixed_in_is_error() {
        let err = InvoiceRegistrationNo::parse("T 123456789012").unwrap_err();
        // 長さは13のままだが数字以外（空白）を含む。
        assert!(matches!(err, JpError::InvoiceRegNoNonDigit { .. }));
    }

    #[test]
    fn parse_check_digit_off_by_one_is_error() {
        // 検査用数字を7→8に変えて基礎番号はそのまま。期待値は7のまま不一致。
        let broken = "T8123456789012";
        let err = InvoiceRegistrationNo::parse(broken).unwrap_err();
        match err {
            JpError::InvoiceRegNoCheckDigit {
                expected, actual, ..
            } => {
                assert_eq!(expected, 7);
                assert_eq!(actual, 8);
            }
            other => panic!("InvoiceRegNoCheckDigit を期待したが {other:?} だった"),
        }
    }

    #[test]
    fn parse_empty_string_is_error() {
        let err = InvoiceRegistrationNo::parse("").unwrap_err();
        assert!(matches!(err, JpError::InvoiceRegNoMissingPrefix { .. }));
    }

    #[test]
    fn parse_leading_whitespace_is_error() {
        // 先頭が空白なので "T" から始まらない。トリムしない方針（doc参照）。
        let err = InvoiceRegistrationNo::parse(" T7123456789012").unwrap_err();
        assert!(matches!(err, JpError::InvoiceRegNoMissingPrefix { .. }));
    }

    #[test]
    fn parse_trailing_whitespace_is_error() {
        // "T" の後が14文字（13桁+末尾空白）になるため桁数エラーになる。
        let err = InvoiceRegistrationNo::parse("T7123456789012 ").unwrap_err();
        assert!(matches!(
            err,
            JpError::InvoiceRegNoWrongLength { actual_len: 14, .. }
        ));
    }

    #[test]
    fn as_str_and_corporate_number_round_trip() {
        let parsed = InvoiceRegistrationNo::parse(VALID).unwrap();
        assert_eq!(parsed.as_str(), "T7123456789012");
        assert_eq!(parsed.corporate_number(), "7123456789012");
        assert_eq!(format!("T{}", parsed.corporate_number()), parsed.as_str());
    }

    // ---- プロパティテスト ----
    //
    // `PROGRESS.md` Phase 0 の教訓（生成器は「型が表現できる範囲」ではなく
    // 「仕様が許容する範囲」に合わせる）に従い、基礎番号12桁の全域
    // （000000000000〜999999999999）から生成する。境界値（全部0・全部9・
    // 最小非ゼロ・末尾のみ9等）は `prop_oneof!` で明示的に含める。
    use proptest::prelude::*;

    /// 基礎番号12桁の全域から生成する。境界値は明示的に含める。
    fn any_base_12_digits() -> impl Strategy<Value = String> {
        prop_oneof![
            8 => (0u64..=999_999_999_999u64).prop_map(|n| format!("{n:012}")),
            1 => Just("000000000000".to_string()),
            1 => Just("999999999999".to_string()),
            1 => Just("000000000001".to_string()),
            1 => Just("100000000000".to_string()),
            1 => Just("999999999998".to_string()),
        ]
    }

    proptest! {
        /// 基礎番号12桁の全域から生成し、正しいチェックデジットを付けた番号は
        /// 必ず `parse` が通ること。
        #[test]
        fn parse_accepts_any_base_with_correct_check_digit(base in any_base_12_digits()) {
            let check_digit = compute_check_digit(&base);
            let candidate = format!("T{check_digit}{base}");
            let parsed = InvoiceRegistrationNo::parse(&candidate);
            prop_assert!(
                parsed.is_ok(),
                "base={base} check_digit={check_digit} candidate={candidate} err={:?}",
                parsed.err()
            );
            prop_assert_eq!(
                parsed.unwrap().corporate_number().to_string(),
                format!("{check_digit}{base}")
            );
        }

        /// チェックデジットを1つずらした番号は必ず弾かれること。
        #[test]
        fn parse_rejects_any_base_with_wrong_check_digit(
            base in any_base_12_digits(),
            offset in 1u32..9u32,
        ) {
            let correct = compute_check_digit(&base);
            // 検査用数字は 1..=9 の範囲（9 - (0..=8)）。1..9 のオフセットを
            // 9 を法として足すことで、必ず correct とは異なる 1..=9 の値になる。
            let wrong = ((correct - 1 + offset) % 9) + 1;
            prop_assert_ne!(wrong, correct);
            let candidate = format!("T{wrong}{base}");
            let result = InvoiceRegistrationNo::parse(&candidate);
            prop_assert!(
                result.is_err(),
                "base={base} wrong={wrong} candidate={candidate} は弾かれるべき"
            );
            let err = result.unwrap_err();
            prop_assert!(
                matches!(err, JpError::InvoiceRegNoCheckDigit { .. }),
                "InvoiceRegNoCheckDigit を期待したが {err:?} だった"
            );
        }
    }
}
