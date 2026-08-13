//! 青色申告決算書（一般用）への当てはめ表（[`BlueReturnForm`]）。
//!
//! `docs/10-report.md` §5。同梱の当てはめ表は
//! `kaikei_jp_data::STATEMENT_BLUE_RETURN_GENERAL`。
//!
//! # これは様式そのものではない（`CLAUDE.md` §10）
//!
//! ここが持つのは「どの勘定科目をどの欄に足すか」だけである。決算書の帳票
//! （国税庁の様式を模した PDF）は作らない。出力は欄番号・欄名・金額の
//! **データ**までとする。
//!
//! # 金額はここでは計算しない
//!
//! この module は表を**読むだけ**で、試算表を受け取って金額を埋めることは
//! しない。当てはめ（どの科目がどの欄か）と集計（いくらか）を分けておくと、
//! 「当てはめが正しいか」を試算表なしで検査できる。集計は `kaikei-report`
//! の決算書出力が行う。
//!
//! # 未マッピングは黙って捨てない
//!
//! [`BlueReturnForm::unmapped_accounts`] が、科目表にあってこの表に無い科目を
//! 返す。呼び出し側はこれを**利用者に見せる**こと。決算書に載らない科目が
//! あることに決算まで気づけないのが、この表で最も危ない失敗である。
//!
//! 「載せないと決めた」科目（`excluded`）は未マッピングに含めない。
//! **「まだ当てはめていない」と「載せないと決めた」は別のこと**であり、
//! 混ぜると前者を見落とす。

use crate::error::JpError;
use kaikei_core::{AccountCode, AccountType, ChartOfAccounts};
use kaikei_jp_data::EmbeddedYaml;
use serde::Deserialize;
use std::collections::BTreeSet;
use std::path::Path;

/// この crate が読める唯一のスキーマ版（`chart.rs` と同じ方針）。
const SUPPORTED_VERSION: u32 = 1;

/// 決算書の欄1つ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormField {
    /// 様式の丸番号（①なら 1）。
    pub no: u32,
    /// 様式に印字されている欄名。**空欄の行では `None`**
    /// （利用者が科目名を書き込む行なので、欄名も帳簿の科目から埋まる）。
    pub label: Option<String>,
    /// この欄に足す勘定科目。
    pub accounts: Vec<AccountCode>,
    /// 他の欄から計算する欄の計算式（様式の印字をそのまま写したもの）。
    ///
    /// この module は式を**解釈しない**（文字列のまま持つ）。計算は
    /// `kaikei-report` の決算書出力が行う。ここで評価器を持つと、
    /// 「表を読めること」と「式を計算できること」が同じ変更で壊れる。
    pub computed: Option<String>,
    /// 帳簿からは決まらず、利用者が指定する値の名前。
    ///
    /// 青色申告特別控除額がこれにあたる。控除の要件（複式簿記・e-Tax申告・
    /// 優良な電子帳簿保存）を満たすかは**ソフトが判定しない**。
    pub from_input: Option<String>,
}

/// 意図して決算書に載せない科目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExcludedAccount {
    /// 科目コード。
    pub account: AccountCode,
    /// 載せない理由。**利用者に見せる**ことを前提にした文章。
    pub reason: String,
}

/// 青色申告決算書への当てはめ表。
#[derive(Debug, Clone)]
pub struct BlueReturnForm {
    form: String,
    part: String,
    fields: Vec<FormField>,
    excluded: Vec<ExcludedAccount>,
}

impl BlueReturnForm {
    /// 様式名（例: 「青色申告決算書（一般用）」）。
    pub fn form(&self) -> &str {
        &self.form
    }

    /// 様式のどの部分か（例: 「損益計算書」）。
    pub fn part(&self) -> &str {
        &self.part
    }

    /// 欄の一覧（様式に印字されている順）。
    pub fn fields(&self) -> &[FormField] {
        &self.fields
    }

    /// 意図して載せない科目の一覧。
    pub fn excluded(&self) -> &[ExcludedAccount] {
        &self.excluded
    }

    /// この表がどこかの欄に当てはめている科目の集合。
    fn mapped_accounts(&self) -> BTreeSet<&AccountCode> {
        self.fields
            .iter()
            .flat_map(|field| field.accounts.iter())
            .collect()
    }

    /// 科目表にあって、この表のどの欄にも当てはまらない損益科目。
    ///
    /// **`excluded` に挙げた科目は含めない**（載せないと決めたものなので）。
    /// 貸借科目（資産・負債・純資産）も含めない——この表は損益計算書の
    /// 当てはめであり、貸借対照表は別の表が扱う。
    ///
    /// 戻り値は科目コード順。**空であることを確かめるのが呼び出し側の仕事**
    /// ではなく、空でなければ利用者に見せるのが仕事である。利用者が独自に
    /// 足した科目がここに出るのは正常であり、エラーではない。
    pub fn unmapped_accounts(&self, chart: &ChartOfAccounts) -> Vec<AccountCode> {
        let mapped = self.mapped_accounts();
        let excluded: BTreeSet<&AccountCode> =
            self.excluded.iter().map(|entry| &entry.account).collect();

        let mut missing: Vec<AccountCode> = chart
            .iter()
            .filter(|def| {
                matches!(
                    def.account_type,
                    AccountType::Revenue | AccountType::Expense
                )
            })
            .map(|def| &def.code)
            .filter(|code| !mapped.contains(code) && !excluded.contains(code))
            .cloned()
            .collect();
        missing.sort_unstable();
        missing
    }

    /// 空欄（`label` が `None`）の欄。
    ///
    /// 様式の空欄は行数が決まっている（一般用の経費なら6行）。利用者が
    /// 独自の経費科目を7つ以上持つと**収まらない**ので、出力側がその状況を
    /// 検出できるように公開する。
    pub fn blank_fields(&self) -> impl Iterator<Item = &FormField> {
        self.fields.iter().filter(|field| field.label.is_none())
    }
}

/// 埋め込み YAML から当てはめ表を読み込む。
pub fn load_embedded(embedded: EmbeddedYaml) -> Result<BlueReturnForm, JpError> {
    let raw: FormRaw = crate::yaml::load_embedded(embedded)?;
    from_raw(embedded.label, raw)
}

/// 任意のファイルパスから読み込む（利用者が当てはめを差し替える経路）。
pub fn load_from_path(path: &Path) -> Result<BlueReturnForm, JpError> {
    let raw: FormRaw = crate::yaml::load_from_path(path)?;
    from_raw(&path.display().to_string(), raw)
}

/// YAML 文字列から読み込む（テスト、および上2つの共通経路）。
pub fn load_from_str(source: &str, label: &str) -> Result<BlueReturnForm, JpError> {
    let raw: FormRaw = crate::yaml::load_str(source, label)?;
    from_raw(label, raw)
}

fn from_raw(label: &str, raw: FormRaw) -> Result<BlueReturnForm, JpError> {
    let invalid = |reason: String| JpError::InvalidChart {
        label: label.to_string(),
        reason,
    };

    if raw.version != SUPPORTED_VERSION {
        return Err(invalid(format!(
            "対応していないスキーマバージョンです: {}（対応: {}）",
            raw.version, SUPPORTED_VERSION
        )));
    }

    let mut fields = Vec::with_capacity(raw.fields.len());
    let mut seen_no = BTreeSet::new();
    for field in raw.fields {
        // **欄番号の重複を通さない。** 同じ番号の欄が2つあると、どちらの
        // 金額が決算書に載るかが読む人にも実装にも決められない。
        if !seen_no.insert(field.no) {
            return Err(invalid(format!("欄番号 {} が重複しています", field.no)));
        }

        // 金額の出どころが2つある欄は、どちらを使うかが決まらない。
        let sources = [
            !field.accounts.is_empty(),
            field.computed.is_some(),
            field.from_input.is_some(),
        ]
        .iter()
        .filter(|present| **present)
        .count();
        if sources > 1 {
            return Err(invalid(format!(
                "欄 {} が accounts / computed / from_input を複数持っています。\
                 金額の出どころは1つにしてください",
                field.no
            )));
        }

        let accounts = field
            .accounts
            .iter()
            .map(|code| {
                AccountCode::parse(code)
                    .map_err(|source| format!("欄 {}: 科目コードが不正です: {source}", field.no))
            })
            .collect::<Result<Vec<_>, String>>()
            .map_err(invalid)?;

        fields.push(FormField {
            no: field.no,
            label: field.label,
            accounts,
            computed: field.computed,
            from_input: field.from_input,
        });
    }

    let excluded = raw
        .excluded
        .into_iter()
        .map(|entry| {
            let account = AccountCode::parse(&entry.account).map_err(|source| {
                format!(
                    "excluded の科目コードが不正です: \"{}\": {source}",
                    entry.account
                )
            })?;
            Ok(ExcludedAccount {
                account,
                reason: entry.reason,
            })
        })
        .collect::<Result<Vec<_>, String>>()
        .map_err(invalid)?;

    // 同じ科目が「欄に当てはめる」と「載せない」の両方に出ていたら、
    // どちらが意図か決められない。
    let mapped: BTreeSet<&AccountCode> = fields
        .iter()
        .flat_map(|field| field.accounts.iter())
        .collect();
    if let Some(conflict) = excluded
        .iter()
        .find(|entry| mapped.contains(&entry.account))
    {
        return Err(invalid(format!(
            "科目 {} が欄への当てはめと excluded の両方にあります",
            conflict.account.as_str()
        )));
    }

    Ok(BlueReturnForm {
        form: raw.form,
        part: raw.part,
        fields,
        excluded,
    })
}

/// 当てはめ表の YAML 上の生の形。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FormRaw {
    version: u32,
    form: String,
    part: String,
    /// 出典。人間が様式を確かめるための文字列で、ドメイン変換では使わない。
    #[allow(dead_code)]
    source: String,
    fields: Vec<FieldRaw>,
    #[serde(default)]
    excluded: Vec<ExcludedRaw>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FieldRaw {
    no: u32,
    /// 空欄の行では `null`。
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    accounts: Vec<String>,
    #[serde(default)]
    computed: Option<String>,
    #[serde(default)]
    from_input: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExcludedRaw {
    account: String,
    reason: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn embedded_form() -> BlueReturnForm {
        load_embedded(kaikei_jp_data::STATEMENT_BLUE_RETURN_GENERAL).unwrap()
    }

    fn embedded_chart() -> ChartOfAccounts {
        crate::chart::load_embedded(kaikei_jp_data::CHART_SOLE_PROPRIETOR).unwrap()
    }

    // BR-1: 同梱の当てはめ表が読めること。
    #[test]
    fn the_embedded_form_parses() {
        let form = embedded_form();
        assert_eq!(form.form(), "青色申告決算書（一般用）");
        assert_eq!(form.part(), "損益計算書");
        assert!(!form.fields().is_empty());
    }

    // BR-2: **本命。** 同梱の科目表の損益科目は、すべてどこかの欄に
    //       当てはまっているか、載せないと明示されているかのどちらか。
    //
    //       科目テンプレートに科目を足したのに当てはめを忘れると、
    //       決算書からその科目が黙って消える。**それは決算まで気づけない。**
    #[test]
    fn every_profit_and_loss_account_is_either_mapped_or_excluded() {
        let form = embedded_form();
        let chart = embedded_chart();

        let unmapped = form.unmapped_accounts(&chart);

        assert!(
            unmapped.is_empty(),
            "同梱の科目表に、決算書のどの欄にも当てはまらない損益科目があります: {:?}。\
             blue_return_general.yaml の fields に足すか、載せないなら excluded に\
             理由を添えて挙げてください",
            unmapped.iter().map(|c| c.as_str()).collect::<Vec<_>>()
        );
    }

    // BR-3: 受取利息は「載せないと決めた」側にあり、未マッピングには出ない。
    //       利子所得であって事業所得ではないため（YAML の reason を参照）。
    #[test]
    fn interest_income_is_excluded_with_a_reason() {
        let form = embedded_form();

        let entry = form
            .excluded()
            .iter()
            .find(|entry| entry.account.as_str() == "530")
            .expect("受取利息が excluded にあるはず");
        assert!(
            entry.reason.contains("利子所得"),
            "載せない理由が利用者に伝わる文章になっていること: {}",
            entry.reason
        );

        // 未マッピングには出ない（「載せないと決めた」と「まだ当てはめて
        // いない」を混ぜない）。
        let unmapped = form.unmapped_accounts(&embedded_chart());
        assert!(!unmapped.iter().any(|code| code.as_str() == "530"));
    }

    // BR-4: 空欄は様式どおり6行ある（一般用の経費 ㉕〜㉚）。
    #[test]
    fn the_form_has_the_six_blank_expense_rows_of_the_official_layout() {
        let form = embedded_form();

        let blanks: Vec<u32> = form.blank_fields().map(|field| field.no).collect();

        assert_eq!(
            blanks,
            vec![25, 26, 27, 28, 29, 30],
            "様式（令和7年分・一般用）の空欄は ㉕〜㉚ の6行"
        );
    }

    // BR-5: 欄番号の重複は拒否する（どちらの金額が載るか決められない）。
    #[test]
    fn duplicate_field_numbers_are_rejected() {
        let source = r#"
version: 1
form: "test"
part: "test"
source: "test"
fields:
  - no: 8
    label: "租税公課"
    accounts: ["600"]
  - no: 8
    label: "荷造運賃"
    accounts: ["601"]
"#;

        let err = load_from_str(source, "test").expect_err("重複は拒否されるはず");
        assert!(format!("{err}").contains("重複"), "{err}");
    }

    // BR-6: 金額の出どころが2つある欄は拒否する。
    #[test]
    fn a_field_with_two_sources_of_its_amount_is_rejected() {
        let source = r#"
version: 1
form: "test"
part: "test"
source: "test"
fields:
  - no: 8
    label: "租税公課"
    accounts: ["600"]
    computed: "1+2"
"#;

        let err = load_from_str(source, "test").expect_err("出どころが2つなら拒否");
        assert!(format!("{err}").contains("金額の出どころ"), "{err}");
    }

    // BR-7: 同じ科目が当てはめと excluded の両方にあったら拒否する。
    #[test]
    fn an_account_that_is_both_mapped_and_excluded_is_rejected() {
        let source = r#"
version: 1
form: "test"
part: "test"
source: "test"
fields:
  - no: 8
    label: "租税公課"
    accounts: ["600"]
excluded:
  - account: "600"
    reason: "test"
"#;

        let err = load_from_str(source, "test").expect_err("両方にあれば拒否");
        assert!(format!("{err}").contains("両方"), "{err}");
    }

    // BR-8: 利用者が科目を足したら未マッピングとして出る（黙って雑費に
    //       寄せない）。この表の一番の役目。
    #[test]
    fn an_account_the_user_added_shows_up_as_unmapped() {
        let form = embedded_form();

        // 同梱の科目表に無い経費科目を利用者が足した状況。
        let chart_source = r#"
version: 1
name: "利用者の科目表"
accounts:
  - { code: "500", name: "売上高", type: Revenue, sort: 500 }
  - { code: "630", name: "研究開発費", type: Expense, sort: 630 }
"#;
        let chart = crate::chart::load_from_str(chart_source, "test").unwrap();

        let unmapped = form.unmapped_accounts(&chart);

        assert_eq!(
            unmapped.iter().map(|c| c.as_str()).collect::<Vec<_>>(),
            vec!["630"],
            "同梱の表に無い科目は、黙って捨てず未マッピングとして返す"
        );
    }

    // BR-9: 未知のスキーマ版は拒否する（chart.rs と同じ方針）。
    #[test]
    fn an_unsupported_schema_version_is_rejected() {
        let source = r#"
version: 99
form: "test"
part: "test"
source: "test"
fields: []
"#;

        let err = load_from_str(source, "test").expect_err("未知のバージョンは拒否");
        assert!(format!("{err}").contains("バージョン"), "{err}");
    }
}
