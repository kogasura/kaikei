//! 合成ルート。**本番で最初の合成ルートがここになる**
//! （`docs/07-mcp-server.md` §4「起動時の組み立て」）。
//!
//! # 組み立ての入口は `kaikei_jp::compose::compose` ただ1つ
//!
//! YAML のロード（勘定科目テンプレート・タグスキーマ・税区分マスタ）を
//! この層で書き直さない。同じ組み立てが複製されると必ず腐る
//! （`DECISIONS.md` D-047 / D-068）。`kaikei-e2e` にも同じ組み立てが
//! あるように見えるが、あちらは `kaikei_jp::compose` の再エクスポートで
//! あり、実装は1つである。**`kaikei-e2e` に依存してはならない**
//! （D-068。依存された瞬間にあの crate の位置づけが壊れる）。
//!
//! # 起動時に落とす（ツール応答に到達させない）
//!
//! `docs/07-mcp-server.md` §7 のとおり、設定・マスタの不備は**起動時に
//! 検出して起動を中止する**。ここで通してしまうと、AI から見ると
//! 「サーバはあるのにどのツールも失敗する」状態になり、原因が設定である
//! ことを AI 自身が知る手段が無い。
//!
//! 起動時に落とすもの:
//!
//! 1. 事業者設定の不足（[`crate::config`]）
//! 2. **決算振替の消費税区分コードが税区分マスタに無い**
//!    （[`validate_closing_tax_category`]）
//! 3. 同梱マスタのロード失敗・決算科目の不在（`compose`）
//! 4. `defaults_as_of` に有効な税区分マスタが無い（`compose`）
//! 5. DB へ接続できない
//! 6. **接続ロールが帳簿への `UPDATE` / `DELETE` / `TRUNCATE` を持っている**（下記）
//! 7. 勘定科目マスタの投入に失敗した
//!
//! # ログは stderr
//!
//! stdio トランスポートでは stdout が JSON-RPC 専用チャネルであり、
//! 1行でも混ざるとプロトコルが壊れる（`docs/07-mcp-server.md` §4）。
//! このモジュールは**何も出力しない**。診断は
//! [`Startup::diagnostics`] として返し、出力先の判断は `main.rs` に
//! 一本化する。

use crate::config::{
    ServerConfig, ENV_APP_DATABASE_URL, ENV_CLOSING_ACCOUNT_CAPITAL,
    ENV_CLOSING_ACCOUNT_OWNER_CONTRIBUTIONS, ENV_CLOSING_ACCOUNT_OWNER_DRAWINGS,
    ENV_CLOSING_TAX_CATEGORY,
};
use kaikei_app::clock::SystemClock;
use kaikei_app::context::BookSettings;
use kaikei_app::id::UuidV7IdGenerator;
use kaikei_app::tx::with_tx;
use kaikei_app::usecase::import_chart::{self, ChartDifference};
use kaikei_core::{AccountingDate, Clock};
use kaikei_jp::compose::{compose, ComposeError, ComposeOptions, Composition};
use kaikei_jp::error::JpError;
use kaikei_jp::tax::TaxRuleSets;
use kaikei_store::audit::PgAuditSink;
use kaikei_store::convert::{naive_date_to_accounting_date, timestamp_to_datetime};
use kaikei_store::pool::{connect_app_with, inspect_journal_privileges, PgStore};
use kaikei_store::query::PgTrialBalanceQuery;
use std::fmt;
use std::sync::Arc;

/// 合成ルートが組み立てた実行時依存一式。
///
/// ツール（PR-F / PR-G）はここから依存を取る。**ツールごとに `PgStore` を
/// 作り直したり `compose` を呼び直したりしないこと**（起動時に一度だけ
/// 組み立てる、が `DECISIONS.md` D-025 / D-057 の前提）。
///
/// ただし `JpStatementPolicy` はここに含めない。決算書を組み立てる直前に
/// その時点の `chart` を読み直して都度構築する（`DECISIONS.md` D-069）。
pub struct Runtime {
    /// 帳簿の読み書き（`kaikei_app` ロール）。
    ///
    /// trait object にせず具象型を持つ（`DECISIONS.md` D-029）。
    pub store: Arc<PgStore>,

    /// 試算表の read model（**`Store` / `Tx` を経由しない**）。
    ///
    /// `CLAUDE.md` §6「read model は物理的に分離する。Repository を通さず
    /// SQL から DTO へ直行する」。書き込み側（[`Runtime::store`]）とは
    /// 別の入口としてここに置く（PR-G。`DECISIONS.md` D-086）。
    pub trial_balance: Arc<PgTrialBalanceQuery>,

    /// 監査ログの記録先。**帳簿とは別のコネクション**で書く
    /// （`DECISIONS.md` D-070 / D-075）。
    pub audit_sink: Arc<PgAuditSink>,

    /// `kaikei-jp` の組み立て結果（勘定科目テンプレート・タグ定義・
    /// 税額計算 policy・決算 policy）。
    ///
    /// # なぜ `Arc` で持つのか（PR-F）
    ///
    /// `kaikei_app::tx::with_tx` / `with_tx_err` に渡すクロージャは HRTB で
    /// 全称量化されており、**`'static` でない借用をキャプチャできない**
    /// （`crates/kaikei-app/src/tx.rs` の doc「クロージャに渡せるもの」）。
    /// 素の `Composition` のままだと、記帳のたびに `JpTaxPolicy`（＝税区分
    /// マスタ一式）を丸ごと `clone` して持ち込むことになる。
    /// `Arc` にしておけば複製は参照カウントの増加で済む。
    pub composition: Arc<Composition>,

    /// 帳簿全体の設定（帳簿通貨・会計年度の区切り規則）。
    pub book_settings: BookSettings,

    /// 仕訳IDの生成（UUID v7）。
    pub id_gen: UuidV7IdGenerator,

    /// 記帳時刻の取得。`Utc::now()` を直に呼ばずこれを通す（`CLAUDE.md` §7）。
    pub clock: SystemClock,

    /// 起動時の科目投入で、**テンプレートと定義が食い違ったため既存を残した**
    /// 科目（`DECISIONS.md` D-081 の `ImportChartOutput::kept_existing`）。
    ///
    /// # なぜ実行時依存として持つのか（PR-E からの申し送り）
    ///
    /// `docs/07-mcp-server.md` §7「PR-G への申し送り」より: この食い違いの
    /// **唯一の出口が stderr** だった。`DECISIONS.md` D-082 は「未設定を
    /// 警告付きで既定値にする」案を「警告は stderr にしか出ず、AI にも
    /// 利用者にも届かない（MCP クライアントがサーバの stderr を表示する
    /// 保証は無い）」という理由で却下しており、同じ理由がここにも当てはまる。
    ///
    /// `get_settings` がこれを返すことで、AI は `list_accounts` が返す名称が
    /// テンプレートと違う理由を自分で説明できるようになる。
    pub chart_differences: Vec<ChartDifference>,
}

/// [`assemble`] の結果。
pub struct Startup {
    /// ツールが使う実行時依存。
    pub runtime: Arc<Runtime>,

    /// 起動時の診断（勘定科目マスタの投入結果、既存を優先した科目など）。
    ///
    /// **stdout に出さないこと。** 出力は `main.rs` が stderr に対して行う。
    pub diagnostics: Vec<String>,
}

/// 設定から実行時依存一式を組み立てる。
///
/// # Errors
///
/// モジュール doc「起動時に落とす」の 2〜6 のいずれかに該当した場合。
/// メッセージはそのまま stderr に出せる日本語である。
pub async fn assemble(config: &ServerConfig) -> Result<Startup, StartupError> {
    let clock = SystemClock;
    let as_of = today(&clock)?;

    // 1. kaikei-jp の組み立て（YAML ロード → policy 構築）。
    let rule_sets = TaxRuleSets::from_embedded().map_err(|source| {
        StartupError::new(format!(
            "同梱の消費税区分マスタを読み込めませんでした: {source}"
        ))
    })?;
    // 決算振替の税区分コードは `compose` の先（`JpSoleProprietorClosingPolicy`）
    // でもマスタと照合されないため、ここで語彙を検証する。詳細は
    // [`validate_closing_tax_category`]。
    validate_closing_tax_category(&rule_sets, as_of, &config.closing_tax_category)?;
    let composition = compose(ComposeOptions {
        rule_sets,
        settings_overrides: config.settings_overrides,
        defaults_as_of: as_of,
        closing_accounts: config.closing_accounts.clone(),
        closing_tax_category: Some(config.closing_tax_category.clone()),
    })
    .map_err(|source| {
        // `ComposeError` の日本語メッセージは言い換えずそのまま出す
        // （`docs/07-mcp-server.md` §7）。**言い換えない代わりに、その値を
        // どの環境変数から渡したかを添える**（`CLAUDE.md` §11。
        // `ComposeError` は `kaikei-jp` の語彙で書かれており、利用者が触れない
        // Rust の構築関数名を「次の手」として提示してしまうため）。
        let hint = closing_settings_hint(&source, config);
        StartupError::new(format!("起動を中止しました: {source}{hint}"))
    })?;

    // 2. DB 接続（kaikei_app ロール）。
    let pool = connect_app_with(&config.app_database_url, config.connect_timeout)
        .await
        .map_err(|source| {
            // 接続文字列そのものは載せない（パスワードが平文で入る。
            // `docs/07-mcp-server.md` §8）。変数名だけを示す。
            StartupError::new(format!(
                "PostgreSQL に接続できませんでした: {source}\n\
                 環境変数 {ENV_APP_DATABASE_URL} の接続先と、\
                 PostgreSQL が起動しているかを確認してください。"
            ))
        })?;

    // 3. 接続ロールの検査。**環境変数を1つ取り違えるだけで防御が1層消える**
    //    （`docs/07-mcp-server.md` §8）。
    let privileges = inspect_journal_privileges(&pool).await.map_err(|source| {
        StartupError::new(format!(
            "接続ロールの権限を確認できませんでした: {source}\n\
             マイグレーションが適用されているか確認してください\
             （cargo run -p kaikei-store --bin kaikei-migrate）。"
        ))
    })?;
    if !privileges.is_append_only() {
        return Err(StartupError::new(format!(
            "起動を中止しました: 接続ロール {role} が帳簿に対する次の権限を\
             持っています: {granted}。\n\
             このサーバーは記帳した仕訳を更新・削除しない前提で作られており、\
             その前提を DB 権限の層でも守るために kaikei_app ロールで接続します。\n\
             環境変数 {ENV_APP_DATABASE_URL} が kaikei_app ロールを指しているか\
             確認してください（kaikei_migrator はテーブル所有者であり \
             REVOKE をバイパスします）。\n\
             kaikei_app を指しているのにこの表示が出る場合は、その環境で\
             帳簿のテーブルに GRANT が追加されています。\
             crates/kaikei-store/migrations/0003_journal.sql の REVOKE と\
             同じ状態に戻してください。",
            role = privileges.role,
            granted = privileges.describe_granted(),
        )));
    }

    let store = Arc::new(PgStore::new(pool.clone()));
    // read model は書き込み側（`PgStore`）を経由せず、自前で同じプールから
    // 引く（`CLAUDE.md` §6）。
    let trial_balance = Arc::new(PgTrialBalanceQuery::new(pool.clone()));
    let audit_sink = Arc::new(PgAuditSink::new(pool));

    // 4. 勘定科目マスタの投入（追加のみ・冪等。`DECISIONS.md` D-081）。
    let imported = with_tx(store.as_ref(), |tx| {
        let chart = composition.chart.clone();
        Box::pin(async move { import_chart::execute(tx, &chart).await })
    })
    .await
    .map_err(|source| {
        StartupError::new(format!(
            "起動を中止しました: 勘定科目マスタを投入できませんでした: {source}"
        ))
    })?;

    let mut diagnostics = vec![
        format!(
            "帳簿通貨 {} / 会計年度 {} / 経理方式 {} / 端数処理 {}（{}）/ \
             課税事業者 {} / 簡易課税 {}",
            config.book_settings.book_currency.code(),
            kaikei_app::wire::fiscal_year_rule_code(config.book_settings.fiscal_year_rule),
            composition.tax_policy.settings().tax_mode.as_code(),
            kaikei_jp::tax::round_mode_code(composition.tax_policy.settings().rounding),
            composition.tax_policy.settings().rounding_unit.as_code(),
            composition.tax_policy.settings().is_taxable_business,
            composition.tax_policy.settings().simplified_taxation,
        ),
        imported.summary(),
    ];
    // 差異は握り潰さない（既存を残したことと、その内容を必ず知らせる）。
    // **stderr だけを出口にしない**（`Runtime::chart_differences` に持たせ、
    // `get_settings` からも読めるようにする。§7 の PR-G への申し送り）。
    diagnostics.extend(imported.kept_existing.iter().map(|d| d.describe()));

    Ok(Startup {
        runtime: Arc::new(Runtime {
            store,
            trial_balance,
            audit_sink,
            composition: Arc::new(composition),
            book_settings: config.book_settings,
            id_gen: UuidV7IdGenerator,
            clock,
            chart_differences: imported.kept_existing,
        }),
        diagnostics,
    })
}

/// 決算振替のゼロ化明細に付ける消費税区分コードが、**その時点で有効な
/// 消費税区分マスタに実在する**ことを起動時に確かめる。
///
/// # なぜ `compose` に任せられないのか
///
/// `JpSoleProprietorClosingPolicy::new` はこのコードを `TagSet` に詰めて
/// タグスキーマの検証に通すが、スキーマが見るのは「`tax_category` キーが
/// 登録済みで値が文字列であること」までで、**値がマスタに存在するかは
/// 見ない**。したがってこの検証を入れないと、12個の必須設定のうち
/// この1つだけが「空でないこと」しか検査されず、存在しない区分コードでも
/// サーバが正常に起動してしまう。
///
/// Phase 3 には `close_period` が無いので実害は出ないが、決算振替を
/// 実装した Phase で**「起動は通るのに決算だけが落ちる」**形になる。
/// `docs/07-mcp-server.md` §7 が「設定・マスタの不備は起動時に検出して
/// 起動を中止する」と定めているのは、まさにこの形を避けるためである。
///
/// # `as_of` に有効なマスタが無い場合はここで判定しない
///
/// その不備は `compose` が `NoApplicableRuleSetForDefaults` として報告する。
/// 同じ原因で2つのメッセージを出すと、どちらを直せばよいか分からなくなる。
fn validate_closing_tax_category(
    rule_sets: &TaxRuleSets,
    as_of: AccountingDate,
    code: &str,
) -> Result<(), StartupError> {
    let Some(table) = rule_sets.for_date(as_of) else {
        return Ok(());
    };
    if table.categories().any(|category| category.code == code) {
        return Ok(());
    }

    let valid = table
        .categories()
        .map(|category| category.code.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    Err(StartupError::new(format!(
        "起動を中止しました: 環境変数 {ENV_CLOSING_TAX_CATEGORY} に指定された\
         消費税区分コード（{code}）が消費税区分マスタに見つかりません。\n\
         {as_of} 時点で有効なマスタ「{label}」（{range}）の有効な値: {valid}\n\
         このコードは決算振替で収益・費用をゼロにする明細に付きます。\
         どの区分を使うかの判断はこのサーバーでは行いません。",
        as_of = as_of.to_iso_string(),
        label = table.label(),
        range = table.range_display(),
    )))
}

/// `ComposeError` に添える「どの環境変数を直せばよいか」。
///
/// # 言い換えではなく追記である
///
/// `docs/07-mcp-server.md` §7 は `ComposeError` の文言をそのまま出すと
/// 定めている（`kaikei-jp` が持つ理由の説明を presentation 層で書き直すと
/// 必ずずれる）。一方で `ComposeError` は `kaikei-jp` の語彙で書かれており、
/// 「正しい科目コードを `JpSoleProprietorClosingPolicy::new` に指定して
/// ください」という**利用者が触れない Rust の構築関数名**を次の手として
/// 提示する。そこで文言は変えず、**その値をどの環境変数から渡したか**を
/// 後ろに足す（`CLAUDE.md` §11。DB 接続の失敗が
/// `APP_DATABASE_URL` を添えているのと同じ形）。
///
/// 決算設定に由来しない失敗（同梱 YAML の破損、`defaults_as_of` に
/// 有効なマスタが無い等）には何も足さない——直す先が環境変数ではないため。
fn closing_settings_hint(error: &ComposeError, config: &ServerConfig) -> String {
    let caused_by_closing_settings = matches!(
        error,
        ComposeError::Jp(
            JpError::MissingClosingAccount { .. }
                | JpError::NotPostableClosingAccount { .. }
                | JpError::DuplicateClosingAccount { .. }
                | JpError::ClosingTagSchemaMismatch { .. }
        )
    );
    if !caused_by_closing_settings {
        return String::new();
    }

    format!(
        "\n決算処理の設定は次の環境変数から渡しています（= の右が現在の値）。\
         該当するものを見直してください:\n\
         \x20 {capital}（元入金）= {capital_value}\n\
         \x20 {drawings}（事業主貸）= {drawings_value}\n\
         \x20 {contributions}（事業主借）= {contributions_value}\n\
         \x20 {tax_category}（ゼロ化明細の消費税区分）= {tax_category_value}\n\
         勘定科目マスタ側を直す場合は、その科目を DB の accounts に追加してください。",
        capital = ENV_CLOSING_ACCOUNT_CAPITAL,
        capital_value = config.closing_accounts.capital.as_str(),
        drawings = ENV_CLOSING_ACCOUNT_OWNER_DRAWINGS,
        drawings_value = config.closing_accounts.owner_drawings.as_str(),
        contributions = ENV_CLOSING_ACCOUNT_OWNER_CONTRIBUTIONS,
        contributions_value = config.closing_accounts.owner_contributions.as_str(),
        tax_category = ENV_CLOSING_TAX_CATEGORY,
        tax_category_value = config.closing_tax_category,
    )
}

/// 起動時点の日付（UTC）。
///
/// `compose` の `defaults_as_of` に渡す。「今日が何日か」の決定は
/// presentation 層の責務であり、`kaikei-app` の `SystemClock` は
/// `Timestamp` までしか返さない（`crates/kaikei-app/src/clock.rs` の doc。
/// `CLAUDE.md` §7）。
///
/// **この日付が影響するのは「その時点で有効な税区分マスタが同梱されて
/// いるか」だけである。** 事業者設定は全項目を明示必須にした
/// （`DECISIONS.md` D-082）ので、マスタの `settings_defaults` は
/// 1項目も採用されない。したがって UTC と現地時間の最大9時間のずれが
/// 記帳結果を変えることはない（年度マスタは年単位で切り替わる）。
/// **取引日は別物**で、こちらは常にツールの引数として渡ってくる。
fn today(clock: &SystemClock) -> Result<AccountingDate, StartupError> {
    let datetime = timestamp_to_datetime(clock.now())
        .map_err(|source| StartupError::new(format!("現在時刻を解釈できません: {source}")))?;
    naive_date_to_accounting_date(datetime.date_naive())
        .map_err(|source| StartupError::new(format!("現在日付を解釈できません: {source}")))
}

/// 起動に失敗した理由。
///
/// **そのまま stderr に出せる日本語のメッセージ**を1つ持つだけの型。
/// 分類コード（`docs/07-mcp-server.md` §6）を持たないのは、起動失敗が
/// ツール応答として返る経路が存在しない（そもそも起動していない）ため。
#[derive(Debug, Clone)]
pub struct StartupError {
    message: String,
}

impl StartupError {
    /// メッセージから作る。
    pub fn new(message: impl Into<String>) -> Self {
        StartupError {
            message: message.into(),
        }
    }

    /// メッセージ本文。
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for StartupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for StartupError {}

impl From<crate::config::ConfigError> for StartupError {
    fn from(source: crate::config::ConfigError) -> Self {
        StartupError::new(source.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 起動時点の日付が取れること（`compose` の `defaults_as_of` に渡す値）。
    #[test]
    fn today_returns_a_usable_accounting_date() {
        let date = today(&SystemClock).unwrap();
        assert!(date.year() >= 2024, "date = {}", date.to_iso_string());
    }

    // 起動失敗のメッセージが `CLAUDE.md` §10 の禁止表現を含まない。
    #[test]
    fn startup_error_message_is_passed_through_verbatim() {
        let err = StartupError::new("テスト用のメッセージ");
        assert_eq!(err.to_string(), "テスト用のメッセージ");
        assert_eq!(err.message(), "テスト用のメッセージ");
    }
}
