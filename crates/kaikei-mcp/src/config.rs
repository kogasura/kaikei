//! 事業者設定と接続情報の読み込み、および**必須検証**
//! （`docs/07-mcp-server.md` §7）。
//!
//! # 既定値にフォールバックしない
//!
//! **1つでも欠けていたら起動しない。** 課税事業者か・税抜経理か・端数を
//! どう処理するかは、このソフトウェアが代わりに決めてよい種類の設定では
//! ない（`CLAUDE.md` §10）。`DECISIONS.md` D-057 は
//! `JpSettingsOverrides::is_taxable_business` を `Option` にしないと決めた際、
//! 理由を「指定を忘れると免税事業者として扱われ税額行が生成されない、という
//! 会計上の実害が大きい間違いを起こしやすい」と書いている。設定ファイル側で
//! 省略を許して既定値に落とすと、**その `Option` 化をしなかった意味が
//! そっくり消える**。
//!
//! `tax_mode` / `rounding` / `rounding_unit` も同様に必須にした
//! （`DECISIONS.md` D-082。年度マスタの `settings_defaults` に暗黙で
//! 落とさない）。
//!
//! # 読み取り元は環境変数
//!
//! MCP クライアントはサーバを**子プロセスとして spawn** する
//! （`docs/07-mcp-server.md` §8）。設定を渡す標準の経路はクライアント設定
//! ファイルの `env` であり、そこに書いた値は環境変数としてこのプロセスに
//! 届く。専用の設定ファイル形式を別に発明すると、**同じ設定が2箇所に
//! 書ける**状態になり、どちらが効いているか分からなくなる。
//!
//! # ここで組み立てるもの・組み立てないもの
//!
//! このモジュールは**文字列 → 値**の解釈と必須検証だけを行う。
//! 値の語彙（`exclusive` / `floor` / `line` / `calendar_year` …）は
//! `kaikei-jp` / `kaikei-app` が公開している `from_code` を通す
//! （同じ綴りの表をこの層で作らない。`DECISIONS.md` D-072）。
//!
//! 実際の組み立て（YAML ロード → policy 構築 → DB 接続）は
//! [`crate::startup`] の仕事である。

use kaikei_app::context::BookSettings;
use kaikei_app::currency::currency_from_code;
use kaikei_app::wire::fiscal_year_rule_from_code;
use kaikei_core::AccountCode;
use kaikei_jp::closing::ClosingAccounts;
use kaikei_jp::tax::{
    round_mode_from_code, JpSettingsOverrides, RoundingUnit, TaxMode, ROUND_MODE_CODES,
};
use kaikei_store::pool::APP_DEFAULT_ACQUIRE_TIMEOUT;
use std::fmt;
use std::time::Duration;

/// 接続文字列を渡す環境変数（`DECISIONS.md` D-048 の変数分離）。
pub const ENV_APP_DATABASE_URL: &str = "APP_DATABASE_URL";
/// 帳簿通貨のコード。
pub const ENV_BOOK_CURRENCY: &str = "KAIKEI_BOOK_CURRENCY";
/// 会計年度の区切り規則。
pub const ENV_FISCAL_YEAR_RULE: &str = "KAIKEI_FISCAL_YEAR_RULE";
/// 経理方式（税抜 / 税込）。
pub const ENV_TAX_MODE: &str = "KAIKEI_TAX_MODE";
/// 端数処理方式。
pub const ENV_ROUNDING: &str = "KAIKEI_ROUNDING";
/// 端数処理単位。
pub const ENV_ROUNDING_UNIT: &str = "KAIKEI_ROUNDING_UNIT";
/// 課税事業者かどうか。
pub const ENV_IS_TAXABLE_BUSINESS: &str = "KAIKEI_IS_TAXABLE_BUSINESS";
/// 簡易課税を選択しているかどうか。
pub const ENV_SIMPLIFIED_TAXATION: &str = "KAIKEI_SIMPLIFIED_TAXATION";
/// 決算科目（元入金）の科目コード。
pub const ENV_CLOSING_ACCOUNT_CAPITAL: &str = "KAIKEI_CLOSING_ACCOUNT_CAPITAL";
/// 決算科目（事業主貸）の科目コード。
pub const ENV_CLOSING_ACCOUNT_OWNER_DRAWINGS: &str = "KAIKEI_CLOSING_ACCOUNT_OWNER_DRAWINGS";
/// 決算科目（事業主借）の科目コード。
pub const ENV_CLOSING_ACCOUNT_OWNER_CONTRIBUTIONS: &str =
    "KAIKEI_CLOSING_ACCOUNT_OWNER_CONTRIBUTIONS";
/// 決算振替のゼロ化明細に付ける消費税区分コード。
pub const ENV_CLOSING_TAX_CATEGORY: &str = "KAIKEI_CLOSING_TAX_CATEGORY";

/// 必須の環境変数の一覧。
///
/// テストが**3方向**と突き合わせる（`PROGRESS.md` Phase 1 の教訓6
/// 「手で維持する一覧は必ず腐る」）:
///
/// | 突き合わせ先 | テスト |
/// |---|---|
/// | [`ServerConfig::from_lookup`] の実挙動 | `the_required_list_matches_what_from_lookup_actually_demands` |
/// | `.env.example` | `every_required_variable_appears_in_the_env_example` |
/// | `README.md` | `every_required_variable_appears_in_the_readme` |
///
/// 後ろの2つが要るのは、[`ConfigError`] の本文が**その2つを一次情報として
/// 名指ししている**ためである。実装とテストが緑のまま `.env.example` だけが
/// 欠けると、起動失敗メッセージの誘導先が嘘になる（`DECISIONS.md` D-047
/// 「手で維持する一覧は腐る」と同型）。
///
/// **誘導先は項目名だけではない。** README の**節名**も同じ性質を持つので、
/// `the_readme_section_named_by_the_failure_message_exists`（このファイルの
/// [`README_SECTION`]）と `the_readme_sections_named_by_the_env_example_exist`
/// （`.env.example` が本文で名指しする節）が突き合わせる。
pub const REQUIRED_ENV_VARS: &[&str] = &[
    ENV_APP_DATABASE_URL,
    ENV_BOOK_CURRENCY,
    ENV_FISCAL_YEAR_RULE,
    ENV_TAX_MODE,
    ENV_ROUNDING,
    ENV_ROUNDING_UNIT,
    ENV_IS_TAXABLE_BUSINESS,
    ENV_SIMPLIFIED_TAXATION,
    ENV_CLOSING_ACCOUNT_CAPITAL,
    ENV_CLOSING_ACCOUNT_OWNER_DRAWINGS,
    ENV_CLOSING_ACCOUNT_OWNER_CONTRIBUTIONS,
    ENV_CLOSING_TAX_CATEGORY,
];

/// 起動に必要な設定一式。
///
/// # `Debug` は接続文字列を伏せる
///
/// `APP_DATABASE_URL` には DB パスワードが平文で入る
/// （`docs/07-mcp-server.md` §8）。導出した `Debug` をどこかで
/// `{:?}` に流すと、それがそのままログ・監査ログ・エラー本文に載る。
/// 手書きの [`fmt::Debug`] 実装で伏せてある。
pub struct ServerConfig {
    /// `kaikei_app` ロールでの接続文字列。
    pub app_database_url: String,
    /// 帳簿全体の設定（帳簿通貨・会計年度の区切り規則）。
    pub book_settings: BookSettings,
    /// 事業者設定の上書き。**全項目が `Some`**（既定値に落とさない）。
    pub settings_overrides: JpSettingsOverrides,
    /// 決算処理に使う3科目。
    pub closing_accounts: ClosingAccounts,
    /// 決算振替のゼロ化明細に付ける消費税区分コード。
    pub closing_tax_category: String,

    /// 起動時に DB への接続を確保できるまで待つ上限。
    ///
    /// **環境変数からは読まない**（[`REQUIRED_ENV_VARS`] に無い）。
    /// 事業者設定ではなく運用上の待ち時間であり、既定値
    /// （[`kaikei_store::pool::APP_DEFAULT_ACQUIRE_TIMEOUT`]）で困る場面が
    /// 無いため、設定項目を増やしていない（増やせば「取り違えて 0 を書く」
    /// という新しい事故を作る）。
    ///
    /// 公開フィールドにしてあるのは、**到達しない接続先を使うテスト**が
    /// 30 秒待たされないようにするため（`tests/startup_config.rs`）。
    pub connect_timeout: Duration,
}

impl fmt::Debug for ServerConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServerConfig")
            .field("app_database_url", &"(伏せ字)")
            .field("book_settings", &self.book_settings)
            .field("settings_overrides", &self.settings_overrides)
            .field("closing_accounts", &self.closing_accounts)
            .field("closing_tax_category", &self.closing_tax_category)
            .field("connect_timeout", &self.connect_timeout)
            .finish()
    }
}

impl ServerConfig {
    /// 環境変数から読み込む。
    ///
    /// **不足・不正は最初の1件で打ち切らず、全部集めて返す。**
    /// 1件ずつ潰させると、12個の設定を揃えるのに12回起動し直すことになる
    /// （`CLAUDE.md` §11「次の手が分かる文言にする」）。
    ///
    /// # Errors
    ///
    /// 未設定・空文字・値が不正な項目が1つでもあれば [`ConfigError`]。
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(&|name| std::env::var(name).ok())
    }

    /// 任意の参照元から読み込む（テスト用。[`from_env`] の実体）。
    ///
    /// プロセスの環境変数は全テストで共有されるため、`std::env::set_var` を
    /// 使うテストは並列実行で干渉する。参照元を差し替えられる形にして
    /// おけば、その落とし穴を踏まずに済む。
    ///
    /// [`from_env`]: ServerConfig::from_env
    pub fn from_lookup(lookup: &dyn Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let mut problems: Vec<String> = Vec::new();

        let app_database_url = required(
            lookup,
            &mut problems,
            ENV_APP_DATABASE_URL,
            "kaikei_app ロールの接続文字列を設定してください\
             （例: postgres://kaikei_app:******@localhost:5432/kaikei）。\
             kaikei_migrator の接続文字列を渡すと、帳簿の追記のみという制約を \
             DB 権限で守る層が無効になるため起動を拒否します",
        );

        let book_currency = required(
            lookup,
            &mut problems,
            ENV_BOOK_CURRENCY,
            "帳簿通貨のコードを設定してください（例: JPY）。\
             通貨ごとに小数桁数が違うため、既定値では起動しません",
        )
        .and_then(|code| {
            parse_with(
                &mut problems,
                ENV_BOOK_CURRENCY,
                &code,
                currency_from_code(&code),
            )
        });

        let fiscal_year_rule = required(
            lookup,
            &mut problems,
            ENV_FISCAL_YEAR_RULE,
            "会計年度の区切り規則を設定してください（現在の対応値: calendar_year）",
        )
        .and_then(|code| {
            parse_with(
                &mut problems,
                ENV_FISCAL_YEAR_RULE,
                &code,
                fiscal_year_rule_from_code(&code),
            )
        });

        let tax_mode = required(
            lookup,
            &mut problems,
            ENV_TAX_MODE,
            &format!(
                "経理方式を設定してください（{}）。\
                 税抜経理か税込経理かで消費税額の行が生成されるかどうかが変わります",
                TaxMode::CODES.join(" / ")
            ),
        )
        .and_then(|code| {
            parse_with(
                &mut problems,
                ENV_TAX_MODE,
                &code,
                TaxMode::from_code(&code),
            )
        });

        let rounding = required(
            lookup,
            &mut problems,
            ENV_ROUNDING,
            &format!(
                "端数処理方式を設定してください（{}）",
                ROUND_MODE_CODES.join(" / ")
            ),
        )
        .and_then(|code| {
            parse_with(
                &mut problems,
                ENV_ROUNDING,
                &code,
                round_mode_from_code(&code),
            )
        });

        let rounding_unit = required(
            lookup,
            &mut problems,
            ENV_ROUNDING_UNIT,
            &format!(
                "端数処理の単位を設定してください（{}）",
                RoundingUnit::CODES.join(" / ")
            ),
        )
        .and_then(|code| {
            parse_with(
                &mut problems,
                ENV_ROUNDING_UNIT,
                &code,
                RoundingUnit::from_code(&code),
            )
        });

        let is_taxable_business = required_bool(
            lookup,
            &mut problems,
            ENV_IS_TAXABLE_BUSINESS,
            "課税事業者なら true、免税事業者なら false を設定してください。\
             どちらであるかの判断はこのサーバーでは行いません。\
             未設定のまま既定値で起動すると、誤った前提のまま\
             消費税額の行が生成される（あるいは生成されない）ことになります",
        );

        let simplified_taxation = required_bool(
            lookup,
            &mut problems,
            ENV_SIMPLIFIED_TAXATION,
            "簡易課税を選択しているなら true、そうでなければ false を\
             設定してください。どちらであるかの判断はこのサーバーでは行いません",
        );

        let capital = required_account_code(
            lookup,
            &mut problems,
            ENV_CLOSING_ACCOUNT_CAPITAL,
            "元入金の科目コードを設定してください（同梱テンプレートでは 400）",
        );
        let owner_drawings = required_account_code(
            lookup,
            &mut problems,
            ENV_CLOSING_ACCOUNT_OWNER_DRAWINGS,
            "事業主貸の科目コードを設定してください（同梱テンプレートでは 410）",
        );
        let owner_contributions = required_account_code(
            lookup,
            &mut problems,
            ENV_CLOSING_ACCOUNT_OWNER_CONTRIBUTIONS,
            "事業主借の科目コードを設定してください（同梱テンプレートでは 420）",
        );

        let closing_tax_category = required(
            lookup,
            &mut problems,
            ENV_CLOSING_TAX_CATEGORY,
            "決算振替のゼロ化明細に付ける消費税区分コードを設定してください\
             （同梱の 2026 年度マスタに含まれる候補の1つは NOT_APPLICABLE）。\
             どの区分を使うかの判断はこのサーバーでは行いません",
        );

        if !problems.is_empty() {
            return Err(ConfigError { problems });
        }

        // ここに到達した時点で全項目が `Some`（`problems` が空である＝
        // 1件も欠けていない）。`expect` の文言はその不変条件を述べる。
        let missing = "必須項目が揃っていることは problems が空であることで保証される";
        Ok(ServerConfig {
            app_database_url: app_database_url.expect(missing),
            book_settings: BookSettings {
                fiscal_year_rule: fiscal_year_rule.expect(missing),
                book_currency: book_currency.expect(missing),
            },
            settings_overrides: JpSettingsOverrides {
                tax_mode: Some(tax_mode.expect(missing)),
                rounding: Some(rounding.expect(missing)),
                rounding_unit: Some(rounding_unit.expect(missing)),
                is_taxable_business: is_taxable_business.expect(missing),
                simplified_taxation: simplified_taxation.expect(missing),
            },
            closing_accounts: ClosingAccounts {
                capital: capital.expect(missing),
                owner_drawings: owner_drawings.expect(missing),
                owner_contributions: owner_contributions.expect(missing),
            },
            closing_tax_category: closing_tax_category.expect(missing),
            connect_timeout: APP_DEFAULT_ACQUIRE_TIMEOUT,
        })
    }
}

/// [`ConfigError`] が誘導先として名指しする README の見出し。
///
/// ★**節の名前も誘導先である**★（PR-I）。README の見出しを改名すると、
/// 起動失敗のメッセージは「そんな節は無い」場所を案内し続ける——
/// `.env.example` が欠けたときと同じ「実装は緑のまま誘導先だけが嘘になる」
/// である。定数にしてテスト
/// （`the_readme_section_named_by_the_failure_message_exists`）から
/// README の見出しと突き合わせる。
const README_SECTION: &str = "事業者設定";

/// 未設定・空文字・値が不正な設定項目をまとめて表す。
///
/// **1件ずつではなく全部返す**。起動失敗の stderr を1回見れば、何を
/// 設定すればよいかが全部分かる形にする。
#[derive(Debug, Clone)]
pub struct ConfigError {
    problems: Vec<String>,
}

impl ConfigError {
    /// 個々の問題（1項目1行）。
    pub fn problems(&self) -> &[String] {
        &self.problems
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "起動を中止しました: 事業者設定が揃っていません（既定値では起動しません）。"
        )?;
        writeln!(f)?;
        for problem in &self.problems {
            writeln!(f, "  - {problem}")?;
        }
        writeln!(f)?;
        writeln!(
            f,
            "設定項目の一覧と例は .env.example と README「{README_SECTION}」を\
             参照してください。"
        )?;
        writeln!(
            f,
            "MCP クライアントから起動する場合は、クライアント設定（Claude Code なら \
             .mcp.json）の env に書きます。"
        )?;
        write!(
            f,
            "課税事業者かどうか・税抜経理かどうかといった判断は、このサーバーでは\
             行いません。既定値で起動すると誤った前提のまま記帳が進むため、\
             1項目でも欠けている間は起動しません。"
        )
    }
}

impl std::error::Error for ConfigError {}

/// 必須の文字列項目を読む。未設定・空白のみは `problems` に積んで `None`。
fn required(
    lookup: &dyn Fn(&str) -> Option<String>,
    problems: &mut Vec<String>,
    name: &str,
    guidance: &str,
) -> Option<String> {
    match lookup(name) {
        None => {
            problems.push(format!("環境変数 {name} が未設定です。{guidance}"));
            None
        }
        // 空文字が「設定した」ことになる事故（設定ファイルに `"KEY": ""` と
        // 書く、シェルで `KEY=` と書く）は実際によく起きる。未設定と同じ
        // 扱いにしつつ、文言では区別する（原因の特定が速くなる）。
        Some(value) if value.trim().is_empty() => {
            problems.push(format!(
                "環境変数 {name} が空です（値が設定されていません）。{guidance}"
            ));
            None
        }
        Some(value) => Some(value.trim().to_string()),
    }
}

/// `from_code` 系の結果を `problems` に写す。
fn parse_with<T, E: fmt::Display>(
    problems: &mut Vec<String>,
    name: &str,
    input: &str,
    parsed: Result<T, E>,
) -> Option<T> {
    match parsed {
        Ok(value) => Some(value),
        Err(source) => {
            problems.push(format!(
                "環境変数 {name} の値が不正です（{input}）: {source}"
            ));
            None
        }
    }
}

/// 必須の真偽値項目を読む。`true` / `false` だけを受け付ける。
///
/// `1` / `0` / `yes` / `on` を受けないのは、**受け入れ方言が増えるほど
/// 「設定したつもりで効いていない」事故が増える**ため。誤った綴りは
/// 既定値に落とさずエラーにする。
fn required_bool(
    lookup: &dyn Fn(&str) -> Option<String>,
    problems: &mut Vec<String>,
    name: &str,
    guidance: &str,
) -> Option<bool> {
    let raw = required(lookup, problems, name, guidance)?;
    match raw.as_str() {
        "true" => Some(true),
        "false" => Some(false),
        other => {
            problems.push(format!(
                "環境変数 {name} の値が不正です（{other}）: true または false を\
                 設定してください。{guidance}"
            ));
            None
        }
    }
}

/// 必須の科目コード項目を読む。
///
/// **その科目が勘定科目表に実在するかどうかはここでは見ない。**
/// 実在検証は `JpSoleProprietorClosingPolicy::new`（構築時に検証する。
/// `DECISIONS.md` D-066）が行い、失敗すれば同じく起動が中止される。
fn required_account_code(
    lookup: &dyn Fn(&str) -> Option<String>,
    problems: &mut Vec<String>,
    name: &str,
    guidance: &str,
) -> Option<AccountCode> {
    let raw = required(lookup, problems, name, guidance)?;
    parse_with(problems, name, &raw, AccountCode::parse(&raw))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaikei_app::context::FiscalYearRule;
    use kaikei_core::RoundMode;
    use std::collections::HashMap;

    /// 全項目が揃った参照元。
    fn full() -> HashMap<String, String> {
        [
            (
                ENV_APP_DATABASE_URL,
                "postgres://kaikei_app:pw@localhost/kaikei",
            ),
            (ENV_BOOK_CURRENCY, "JPY"),
            (ENV_FISCAL_YEAR_RULE, "calendar_year"),
            (ENV_TAX_MODE, "exclusive"),
            (ENV_ROUNDING, "floor"),
            (ENV_ROUNDING_UNIT, "line"),
            (ENV_IS_TAXABLE_BUSINESS, "true"),
            (ENV_SIMPLIFIED_TAXATION, "false"),
            (ENV_CLOSING_ACCOUNT_CAPITAL, "400"),
            (ENV_CLOSING_ACCOUNT_OWNER_DRAWINGS, "410"),
            (ENV_CLOSING_ACCOUNT_OWNER_CONTRIBUTIONS, "420"),
            (ENV_CLOSING_TAX_CATEGORY, "NOT_APPLICABLE"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
    }

    fn load(vars: &HashMap<String, String>) -> Result<ServerConfig, ConfigError> {
        ServerConfig::from_lookup(&|name| vars.get(name).cloned())
    }

    // 全項目が揃っていれば読める。
    #[test]
    fn a_complete_environment_produces_a_config() {
        let config = load(&full()).unwrap();
        assert_eq!(config.book_settings.book_currency.code(), "JPY");
        assert_eq!(
            config.book_settings.fiscal_year_rule,
            FiscalYearRule::CalendarYear
        );
        assert_eq!(config.settings_overrides.tax_mode, Some(TaxMode::Exclusive));
        assert_eq!(config.settings_overrides.rounding, Some(RoundMode::Floor));
        assert_eq!(
            config.settings_overrides.rounding_unit,
            Some(RoundingUnit::Line)
        );
        assert!(config.settings_overrides.is_taxable_business);
        assert!(!config.settings_overrides.simplified_taxation);
        assert_eq!(config.closing_accounts.capital.as_str(), "400");
        assert_eq!(config.closing_tax_category, "NOT_APPLICABLE");
    }

    // MC-24: どの1項目が欠けても起動用の設定は組み立てられず、
    // メッセージがその項目を名指しする（既定値に落ちない）。
    #[test]
    fn every_required_variable_is_actually_required() {
        for missing in REQUIRED_ENV_VARS {
            let mut vars = full();
            vars.remove(*missing);
            let err = load(&vars).unwrap_err();
            assert!(
                err.problems().iter().any(|p| p.contains(missing)),
                "{missing} を外したのに、その項目を名指しするメッセージが出ていない: {err}"
            );
            let text = err.to_string();
            assert!(text.contains(missing), "{missing}: {text}");
            assert!(
                text.contains("既定値では起動しません"),
                "既定値に落ちないことが読み取れる文言であること: {text}"
            );
        }
    }

    // `REQUIRED_ENV_VARS` が実装から乖離していないこと（手で維持する
    // 一覧が腐るのを防ぐ。PROGRESS.md Phase 1 の教訓6）。
    #[test]
    fn the_required_list_matches_what_from_lookup_actually_demands() {
        let err = ServerConfig::from_lookup(&|_| None).unwrap_err();
        assert_eq!(
            err.problems().len(),
            REQUIRED_ENV_VARS.len(),
            "何も設定していないときの指摘件数と REQUIRED_ENV_VARS の件数が\
             一致すること: {err}"
        );
        for name in REQUIRED_ENV_VARS {
            assert!(
                err.problems().iter().any(|p| p.contains(name)),
                "{name} が指摘されていない"
            );
        }
    }

    /// リポジトリ直下の `.env.example`（`ConfigError` が誘導先として
    /// 名指ししているファイル）。**コンパイル時に埋め込む**ので、
    /// ファイルが消えればビルドが落ちる。
    const ENV_EXAMPLE: &str = include_str!("../../../.env.example");

    /// リポジトリ直下の `README.md`（同上）。
    const README: &str = include_str!("../../../README.md");

    /// `haystack` に `name` が**識別子として**現れるか。
    ///
    /// 素の `contains` だと、ある項目名が別の項目名の**接頭辞**になっている
    /// ときに検出できない（`KAIKEI_ROUNDING` は `KAIKEI_ROUNDING_UNIT` の
    /// 接頭辞なので、前者を消してもテストが緑のまま通る）。それでは
    /// このテストが塞ごうとしている「実装は緑のまま誘導先だけが嘘になる」
    /// を防げないので、**直後が識別子の構成文字でないこと**まで見る。
    fn mentions_variable(haystack: &str, name: &str) -> bool {
        haystack.match_indices(name).any(|(at, _)| {
            haystack[at + name.len()..]
                .chars()
                .next()
                .is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_')
        })
    }

    // 必須項目が `.env.example` に書かれている。
    //
    // `REQUIRED_ENV_VARS` と `from_lookup` の実挙動は上のテストで
    // 突き合わせてあるが、**利用者が読む側**とは繋がっていなかった。
    // PR-F / PR-G で必須項目が増えたとき、実装とテストは緑のまま
    // `.env.example` だけが欠けると、起動失敗メッセージの誘導先が嘘になる。
    #[test]
    fn every_required_variable_appears_in_the_env_example() {
        for name in REQUIRED_ENV_VARS {
            assert!(
                mentions_variable(ENV_EXAMPLE, name),
                "{name} が .env.example に載っていません。\
                 起動に失敗したときのメッセージは .env.example を一次情報として\
                 案内するため、項目を足したら同じ PR で .env.example にも足すこと"
            );
        }
    }

    // 必須項目が README に書かれている（同上）。
    #[test]
    fn every_required_variable_appears_in_the_readme() {
        for name in REQUIRED_ENV_VARS {
            assert!(
                mentions_variable(README, name),
                "{name} が README.md に載っていません。\
                 起動に失敗したときのメッセージは README を一次情報として\
                 案内するため、項目を足したら同じ PR で README にも足すこと"
            );
        }
    }

    // 上の2つが「接頭辞だけの一致」を通さない。
    //
    // これを置かないと、`mentions_variable` を素の `contains` に
    // 書き戻しても両テストが緑のまま通る（＝ガードのガードが無い）。
    #[test]
    fn a_variable_that_only_appears_as_a_prefix_is_not_counted() {
        assert!(mentions_variable(
            "KAIKEI_ROUNDING=floor",
            "KAIKEI_ROUNDING"
        ));
        assert!(!mentions_variable(
            "KAIKEI_ROUNDING_UNIT=line",
            "KAIKEI_ROUNDING"
        ));
        assert!(mentions_variable(
            "\"KAIKEI_ROUNDING\": \"floor\"",
            "KAIKEI_ROUNDING"
        ));
        // 末尾に現れる場合（直後の文字が無い）も見つける。
        assert!(mentions_variable("… KAIKEI_ROUNDING", "KAIKEI_ROUNDING"));
    }

    // 誘導先の名前が実際のファイル名と一致している（メッセージの中の
    // `.env.example` / README という綴りだけが古くなるのを防ぐ）。
    #[test]
    fn the_failure_message_points_at_files_that_exist() {
        let text = ServerConfig::from_lookup(&|_| None)
            .unwrap_err()
            .to_string();
        assert!(text.contains(".env.example"), "{text}");
        assert!(text.contains("README"), "{text}");
        assert!(!ENV_EXAMPLE.is_empty());
        assert!(!README.is_empty());
    }

    // ★節の名前も誘導先である★（PR-I）
    //
    // 上のテストは「README という文字列が本文に出ること」と「ファイルが
    // 空でないこと」しか見ておらず、**その節が README に在るか**は見て
    // いなかった。実際 PR-I で README を書き直したとき、メッセージが
    // 名指ししていた節（「MCP サーバーを起動する」）は消えていた。
    // 項目を足したのに `.env.example` へ足し忘れるのと同じ形の嘘である。
    #[test]
    fn the_readme_section_named_by_the_failure_message_exists() {
        let heading = README
            .lines()
            .any(|line| line.starts_with('#') && line.contains(README_SECTION));
        assert!(
            heading,
            "起動失敗のメッセージが案内する README の節「{README_SECTION}」が\
             見出しとして存在しません。README の見出しを変えたなら、\
             同じ PR で config.rs の README_SECTION も直すこと"
        );
        assert!(
            ServerConfig::from_lookup(&|_| None)
                .unwrap_err()
                .to_string()
                .contains(README_SECTION),
            "メッセージが README_SECTION を経由していません"
        );
    }

    /// `.env.example` の本文から `README「…」` の形の参照を全部拾う。
    ///
    /// 定数に1つだけ書き写す形にしないのは、**参照が増えたときに検査から
    /// 漏れる**のを防ぐため（漏れた参照はまさに「実装は緑のまま誘導先だけが
    /// 嘘になる」経路である）。
    fn readme_sections_named_by(text: &str) -> Vec<&str> {
        let mut found = Vec::new();
        let mut rest = text;
        while let Some(at) = rest.find("README「") {
            let after = &rest[at + "README「".len()..];
            match after.find('」') {
                Some(end) => {
                    found.push(&after[..end]);
                    rest = &after[end..];
                }
                None => break,
            }
        }
        found
    }

    // ★`.env.example` が名指しする節も誘導先である★
    //
    // `README_SECTION`（起動失敗メッセージの誘導先）は
    // `the_readme_section_named_by_the_failure_message_exists` が見ているが、
    // **もう1つの参照元である `.env.example` は対象外**だった。実際 PR-I が
    // README を書き直したとき、`.env.example` が名指ししていた節
    // 「ローカル開発環境」は消えたまま残っていた。利用者が最初に開くファイル
    // （手順1 の `cp .env.example .env`）に、存在しない節への案内が載る。
    #[test]
    fn the_readme_sections_named_by_the_env_example_exist() {
        let sections = readme_sections_named_by(ENV_EXAMPLE);
        assert!(
            !sections.is_empty(),
            "`.env.example` から README「…」の参照が1つも読み取れません。\
             参照の書き方を変えたなら readme_sections_named_by も直すこと\
             （検査が黙って無意味になります）"
        );
        for section in sections {
            assert!(
                README
                    .lines()
                    .any(|line| line.starts_with('#') && line.contains(section)),
                "`.env.example` が案内する README の節「{section}」が見出しとして\
                 存在しません。README の見出しを変えたなら、同じ PR で\
                 `.env.example` の参照も直すこと"
            );
        }
    }

    // 上のテストが「参照を1つも見つけられないまま緑」にならない
    // （＝ガードのガード）。
    #[test]
    fn a_readme_reference_is_actually_extracted_from_the_text() {
        assert_eq!(
            readme_sections_named_by("… README「テスト」を参照。README「事業者設定」も"),
            vec!["テスト", "事業者設定"]
        );
        assert!(readme_sections_named_by("README を参照").is_empty());
    }

    // 事業者設定は `.env.example` に**値を入れて**配らない（D-057 / D-082）。
    //
    // `cp .env.example .env` して CHANGE_ME を置換しただけで起動できると、
    // 利用者は税抜経理か・課税事業者かを一度も宣言しないまま記帳を始める。
    // 「既定値にフォールバックしない」という決定が、配り方の側で骨抜きに
    // なるのを防ぐ（接続文字列 `APP_DATABASE_URL` は事業者設定ではなく、
    // かつ CHANGE_ME を置換しないと使えないので対象外）。
    #[test]
    fn the_env_example_ships_the_business_settings_commented_out() {
        for name in REQUIRED_ENV_VARS {
            if *name == ENV_APP_DATABASE_URL {
                continue;
            }
            let assigned = ENV_EXAMPLE.lines().any(|line| {
                let line = line.trim_start();
                !line.starts_with('#') && line.starts_with(&format!("{name}="))
            });
            assert!(
                !assigned,
                "{name} が .env.example で有効な行として値を持っています。\
                 事業者設定はコメントアウトして配ること——値を入れて配ると、\
                 CHANGE_ME を置換しただけの利用者が、税抜経理か・課税事業者かを\
                 一度も宣言しないまま起動できてしまいます（D-057 / D-082）"
            );
        }
    }

    // 空文字は「設定した」ことにしない（未設定と同じく起動を止める）。
    #[test]
    fn an_empty_value_is_treated_as_missing() {
        for name in REQUIRED_ENV_VARS {
            let mut vars = full();
            vars.insert((*name).to_string(), "   ".to_string());
            let err = load(&vars).unwrap_err();
            assert!(
                err.problems()
                    .iter()
                    .any(|p| p.contains(name) && p.contains("空です")),
                "{name} を空文字にしたのに空である旨が出ていない: {err}"
            );
        }
    }

    // 不足は1件で打ち切らず全部返す（12回起動し直させない）。
    #[test]
    fn all_missing_items_are_reported_at_once() {
        let mut vars = full();
        vars.remove(ENV_IS_TAXABLE_BUSINESS);
        vars.remove(ENV_TAX_MODE);
        vars.remove(ENV_CLOSING_TAX_CATEGORY);
        let err = load(&vars).unwrap_err();
        assert_eq!(err.problems().len(), 3, "{err}");
    }

    // 真偽値は true / false 以外を既定値に落とさずエラーにする。
    #[test]
    fn boolean_settings_reject_dialects_instead_of_guessing() {
        for value in ["1", "0", "yes", "TRUE", "はい"] {
            let mut vars = full();
            vars.insert(ENV_IS_TAXABLE_BUSINESS.to_string(), value.to_string());
            let err = load(&vars).unwrap_err();
            assert!(
                err.problems()
                    .iter()
                    .any(|p| p.contains(ENV_IS_TAXABLE_BUSINESS)),
                "{value} が受理されている"
            );
        }
    }

    // 未知の通貨コードは桁数を推測せずエラーになる（CLAUDE.md §8）。
    #[test]
    fn an_unknown_currency_code_is_rejected_rather_than_guessed() {
        let mut vars = full();
        vars.insert(ENV_BOOK_CURRENCY.to_string(), "KWD".to_string());
        let err = load(&vars).unwrap_err();
        assert!(err.to_string().contains(ENV_BOOK_CURRENCY), "{err}");
    }

    // 語彙の誤りは、有効な値を列挙して返す（CLAUDE.md §11）。
    #[test]
    fn an_unknown_tax_mode_lists_the_valid_values() {
        let mut vars = full();
        vars.insert(ENV_TAX_MODE.to_string(), "zeinuki".to_string());
        let err = load(&vars).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("exclusive"), "{text}");
        assert!(text.contains("inclusive"), "{text}");
    }

    // 接続文字列は Debug に出さない（docs/07 §8。パスワードが平文で入る）。
    #[test]
    fn debug_output_never_contains_the_connection_string() {
        let config = load(&full()).unwrap();
        let text = format!("{config:?}");
        assert!(!text.contains("pw"), "{text}");
        assert!(!text.contains("postgres://"), "{text}");
    }

    // CLAUDE.md §10: 起動失敗の文言が税務判断を断定していないこと。
    #[test]
    fn the_failure_message_avoids_asserting_tax_conclusions() {
        let err = ServerConfig::from_lookup(&|_| None).unwrap_err();
        let text = err.to_string();
        for forbidden in ["準拠", "法令対応", "JIIMA"] {
            assert!(!text.contains(forbidden), "{forbidden}: {text}");
        }
        assert!(
            text.contains("このサーバーでは") && text.contains("行いません"),
            "判断を利用者に残していることが読み取れる文言であること: {text}"
        );
    }
}
