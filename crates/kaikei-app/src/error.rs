//! アプリケーション層全体で使うエラー型（[`AppError`] / [`RepoError`]）。
//!
//! `CLAUDE.md` §11 の方針（次の手が分かる文言にする）に従う。`RepoError` は
//! ドメイン語彙の enum にする（`Box<dyn Error>` 一本にしない）。永続化層が
//! `Box<dyn Error>` 一本を返す設計だと、append-only 違反（DB権限・トリガによる
//! 拒否）が「ただの DB エラー」の1バリアントに潰れてしまい、この方針を
//! 満たせなくなる。SQLSTATE（`42501` = 権限拒否 / `P0010` = append-only 違反の
//! トリガ / `P0011` = 貸借不一致のトリガ / `23505` = 一意制約 等）の判別は
//! 実装側（`kaikei-store` の sqlstate マッピング）が行い、この enum の適切な
//! バリアントへ写像する（`DECISIONS.md` D-032、および `P0010`/`P0011` を
//! 汎用の `P0001` から分離した D-038）。
//!
//! # エラーコードの語彙（[`codes`]）
//!
//! presentation 層（`kaikei-mcp` / 将来の `kaikei-api`）が応答の `error`
//! フィールドに載せる**機械可読な分類コード**の対応表をこのモジュールに
//! 1箇所だけ置く（`docs/07-mcp-server.md` §6、`DECISIONS.md` D-072）。
//! 引くための入口は次の4つ:
//!
//! | 写像元 | 入口 |
//! |---|---|
//! | [`AppError`] | [`AppError::code`] |
//! | [`RepoError`] | [`RepoError::code`] |
//! | `kaikei_core::CoreError` | [`core_error_code`] |
//! | `kaikei_policy::PolicyError` | [`policy_error_code`] |
//!
//! `CoreError` / `PolicyError` が**メソッドではなく自由関数**なのは、
//! 定義元の crate（`kaikei-core` / `kaikei-policy`）が凍結層であり
//! （`CLAUDE.md` §1）、他 crate から `impl` を生やせないため。
//!
//! # 本文には2つの入口がある（`Display` と `public_message`）
//!
//! | 入口 | 宛先 | 内容 |
//! |---|---|---|
//! | `Display`（`to_string()`） | **内部ログ・診断** | 下位層が返した生の文字列を含みうる |
//! | [`AppError::public_message`] / [`RepoError::public_message`] | **外部への応答**（MCP / HTTP の `message`、`audit_log.output`） | 生の文字列を含まないことを型の側で保証する |
//!
//! この2つを分けるのは、`docs/07-mcp-server.md` §3（「`message` は
//! `Display` を写像したもの」）と §9（「接続文字列を含みうる下位層のエラー
//! 本文をそのまま転記しない」）が**そのままでは両立しない**ため。
//! 実際 `kaikei-store` の `sqlstate::map_sqlstate` は
//! `reason: format!("...: {message}")` として **DB が返した文字列を
//! そのまま埋めている**（接続文字列・ロール名・テーブル定義が混じりうる）。
//!
//! ドメインのエラー（`Unbalanced` / `UnknownAccount` / `EmptyReverseReason` 等）は
//! 文言をこのリポジトリ自身が書いているので `public_message` も `Display` と
//! 同じ値を返す。正規化するのは**下位層の生メッセージを含みうるバリアント
//! だけ**である（[`RepoError::public_message`] の表を参照）。

use crate::id::entry_id_to_uuid_string;
use kaikei_core::{CoreError, EntryId, EntryNumber};
use kaikei_policy::PolicyError;

/// エラーコードの語彙。
///
/// # コードとメッセージは別物
///
/// ここに定義するのは **snake_case の安定した識別子**であり、
/// 各エラーの `Display`（日本語のメッセージ）とは独立している。
///
/// - **メッセージを変えてもコードは変わらない。** 文言の改善
///   （`CLAUDE.md` §11「次の手が分かる文言にする」）は随時行うが、
///   コードは分類の identity なので変えない。
/// - 逆に、コードを変えることは presentation 層・`audit_log.error_code`・
///   それを読む AI の分岐を同時に壊す破壊的変更である。
///   バリアントの意味が変わったときは、コードを付け替えるのではなく
///   **新しいコードを足す**（`CLAUDE.md` §2 のタグキーと同じ扱い）。
/// - コードは翻訳しない。ロケールに依存するのはメッセージだけ。
///
/// # 名前空間は平坦
///
/// 層ごとの接頭辞は付けない（`RepoError::AppendOnlyViolation` は
/// `append_only_violation` であって `repo_append_only_violation` ではない）。
/// AI が読むのは「何が起きたか」であって「どの crate が返したか」ではない。
/// 例外は**異なる層に同名のバリアントが実在する場合**だけで、現状は
/// `Unsupported` の1件（[`POLICY_UNSUPPORTED`] / [`REPO_UNSUPPORTED`]）。
/// 同じコードに潰すと「税区分の未対応」と「証憑紐付けの未実装」が
/// 区別できなくなり、AI が誤った対処に進む。
pub mod codes {
    // ---- kaikei_core::CoreError（15バリアント） ----

    /// `CoreError::Unbalanced`（貸借不一致）。
    pub const UNBALANCED: &str = "unbalanced";
    /// `CoreError::TooFewLines`（明細が2行未満）。
    pub const TOO_FEW_LINES: &str = "too_few_lines";
    /// `CoreError::UnknownAccount`（勘定科目表に無い科目コード）。
    pub const UNKNOWN_ACCOUNT: &str = "unknown_account";
    /// `CoreError::NotPostable`（見出し科目への記帳）。
    pub const NOT_POSTABLE: &str = "not_postable";
    /// `CoreError::CurrencyMismatch`（通貨の混在）。
    pub const CURRENCY_MISMATCH: &str = "currency_mismatch";
    /// `CoreError::InvalidAmount`（金額の桁数超過・パース失敗・オーバーフロー）。
    pub const INVALID_AMOUNT: &str = "invalid_amount";
    /// `CoreError::UnknownTagKey`（`TagSchema` 未登録のタグキー）。
    pub const UNKNOWN_TAG_KEY: &str = "unknown_tag_key";
    /// `CoreError::TagTypeMismatch`（タグ値の型がスキーマと不一致）。
    pub const TAG_TYPE_MISMATCH: &str = "tag_type_mismatch";
    /// `CoreError::MissingRequiredTag`（必須タグの欠落）。
    pub const MISSING_REQUIRED_TAG: &str = "missing_required_tag";
    /// `CoreError::DateOutOfFiscalYear`（取引日が会計年度の範囲外）。
    pub const DATE_OUT_OF_FISCAL_YEAR: &str = "date_out_of_fiscal_year";
    /// `CoreError::PeriodClosed`（締め済み期間への記帳）。
    pub const PERIOD_CLOSED: &str = "period_closed";
    /// `CoreError::EmptyDescription`（摘要が空）。
    pub const EMPTY_DESCRIPTION: &str = "empty_description";
    /// `CoreError::InvalidChart`（勘定科目表そのものが不正）。
    pub const INVALID_CHART: &str = "invalid_chart";
    /// `CoreError::NotAggregatable`（集計軸に使えないタグキー）。
    pub const NOT_AGGREGATABLE: &str = "not_aggregatable";
    /// `CoreError::InvalidValue`（上記以外の値の不正）。
    pub const INVALID_VALUE: &str = "invalid_value";

    // ---- kaikei_policy::PolicyError（8バリアント。`Core` は委譲） ----

    /// `PolicyError::NoApplicableRuleSet`（その取引日に適用できるマスタが無い）。
    pub const NO_APPLICABLE_RULE_SET: &str = "no_applicable_rule_set";
    /// `PolicyError::UnknownTaxCategory`（その日時点のマスタに無い税区分）。
    pub const UNKNOWN_TAX_CATEGORY: &str = "unknown_tax_category";
    /// `PolicyError::TaxCategoryNotApplicable`（その科目に適用できない税区分）。
    pub const TAX_CATEGORY_NOT_APPLICABLE: &str = "tax_category_not_applicable";
    /// `PolicyError::UnknownCounterparty`（取引先マスタに無い取引先コード）。
    pub const UNKNOWN_COUNTERPARTY: &str = "unknown_counterparty";
    /// `PolicyError::QualifiedInvoiceUnverified`（適格請求書発行事業者の登録状況が未確認）。
    pub const QUALIFIED_INVOICE_UNVERIFIED: &str = "qualified_invoice_unverified";
    /// `PolicyError::InvalidPolicyData`（policy が構築時に受け取ったデータが不正）。
    pub const INVALID_POLICY_DATA: &str = "invalid_policy_data";
    /// `PolicyError::Unsupported`（policy が未対応の操作）。
    pub const POLICY_UNSUPPORTED: &str = "policy_unsupported";

    // ---- RepoError（7バリアント） ----

    /// `RepoError::NotFound`（対象が永続化層に存在しない）。
    pub const NOT_FOUND: &str = "not_found";
    /// `RepoError::AppendOnlyViolation`（append-only の権限・トリガによる拒否）。
    pub const APPEND_ONLY_VIOLATION: &str = "append_only_violation";
    /// `RepoError::Conflict`（一意制約違反）。
    pub const CONFLICT: &str = "conflict";
    /// `RepoError::Corrupt`（保存データが不正）。
    pub const CORRUPT: &str = "corrupt";
    /// `RepoError::OutOfRange`（変換先の型で表現できる範囲を超えている）。
    pub const OUT_OF_RANGE: &str = "out_of_range";
    /// `RepoError::Unsupported`（永続化層が未対応の操作）。
    pub const REPO_UNSUPPORTED: &str = "repo_unsupported";
    /// `RepoError::Backend`（接続断等、上記に分類できない永続化層の失敗）。
    pub const BACKEND: &str = "backend";

    // ---- AppError 自身（`Repo` / `Policy` / `Core` は委譲） ----

    /// `AppError::AlreadyReversed`（既に赤伝済みの仕訳の再訂正）。
    pub const ALREADY_REVERSED: &str = "already_reversed";
    /// `AppError::EmptyReverseReason`（訂正理由が空文字・空白のみ）。
    pub const EMPTY_REVERSE_REASON: &str = "empty_reverse_reason";
    /// `AppError::InvalidEntryId`（仕訳IDが UUID の正準表記として解釈できない）。
    ///
    /// **`not_found` とは別のコードにする。** 「その UUID の仕訳が無い」
    /// （＝IDを調べ直す）と「送られた文字列が UUID ですらない」
    /// （＝表記を直す）は AI が取るべき次の手が違う
    /// （`kaikei_app::id::entry_id_from_uuid_string` が返す）。
    pub const INVALID_ENTRY_ID: &str = "invalid_entry_id";
    /// `AppError::Inconsistent`（試算表の検算失敗）。
    pub const INCONSISTENT: &str = "inconsistent";
    /// `AppError::Rejected`（上記に分類できない業務ルール違反）。
    pub const REJECTED: &str = "rejected";

    // ---- presentation 層が直接使うコード（`AppError` のバリアントを持たない） ----

    /// 監査ログの**開始レコードが書けなかった**ため、ツールを実行しなかった
    /// （`docs/07-mcp-server.md` §9 の fail-closed）。
    ///
    /// このコードに対応する [`super::AppError`] のバリアントは**無い**。
    /// fail-closed の判定は `with_tx` の外側（`AuditSink` を呼ぶ presentation
    /// 層。ポート自体は後続 PR）で起き、ユースケースには到達しないため。
    /// 語彙だけを先にここに置くのは、実装者が場当たりの綴りを発明したり
    /// [`REJECTED`] を借りたりするのを防ぐため。
    ///
    /// **[`REJECTED`] を借りてはならない。** `Rejected` は
    /// 「集計期間の開始日が終了日より後です」のような**入力を直せば通る**
    /// 拒否に使っている。同じコードにすると、AI が
    /// 「入力を直せば通るのか」「サーバ都合で今は実行できないのか」を
    /// 区別できず、無意味な入力の作り直しを繰り返す。
    ///
    /// 添えるメッセージは「**帳簿は変更されていません**」まで含めること
    /// （`docs/07-mcp-server.md` §9・`CLAUDE.md` §11）。
    pub const AUDIT_LOG_UNAVAILABLE: &str = "audit_log_unavailable";

    // ---- 受け皿 ----

    /// **下流**が知らないバリアントに出会ったときの既定コード。
    ///
    /// [`super::AppError`] は `#[non_exhaustive]` なので、`kaikei-app` の
    /// 外（`kaikei-mcp` / `kaikei-api`）の `match` にはワイルドカードの腕が
    /// 必須であり、将来追加されたバリアントはそこに落ちる。そのとき実装者が
    /// 場当たりのコードを発明しないよう、既定を1つに決めておく
    /// （`docs/07-mcp-server.md` §6）。
    ///
    /// **`kaikei-app` の中では使わない。** [`super::AppError::code`] は
    /// ワイルドカードを持たない網羅 `match` であり、バリアントを足すと
    /// コンパイルが壊れて割り当て漏れが露見する（`DECISIONS.md` D-072 の
    /// 訂正注記）。
    pub const INTERNAL: &str = "internal";
}

/// `kaikei_core::CoreError` に対応するエラーコードを返す。
///
/// `CoreError` は `#[non_exhaustive]` ではない（凍結層である `kaikei-core` の
/// `src/error.rs` を実測確認済み）ため、ワイルドカードを置かない網羅
/// `match` にする。`kaikei-core` にバリアントが追加されたらこの関数の
/// コンパイルが壊れ、コードの割り当て漏れがビルド時に露見する。
pub fn core_error_code(err: &CoreError) -> &'static str {
    match err {
        CoreError::Unbalanced { .. } => codes::UNBALANCED,
        CoreError::TooFewLines { .. } => codes::TOO_FEW_LINES,
        CoreError::UnknownAccount { .. } => codes::UNKNOWN_ACCOUNT,
        CoreError::NotPostable { .. } => codes::NOT_POSTABLE,
        CoreError::CurrencyMismatch { .. } => codes::CURRENCY_MISMATCH,
        CoreError::InvalidAmount { .. } => codes::INVALID_AMOUNT,
        CoreError::UnknownTagKey { .. } => codes::UNKNOWN_TAG_KEY,
        CoreError::TagTypeMismatch { .. } => codes::TAG_TYPE_MISMATCH,
        CoreError::MissingRequiredTag { .. } => codes::MISSING_REQUIRED_TAG,
        CoreError::DateOutOfFiscalYear { .. } => codes::DATE_OUT_OF_FISCAL_YEAR,
        CoreError::PeriodClosed { .. } => codes::PERIOD_CLOSED,
        CoreError::EmptyDescription => codes::EMPTY_DESCRIPTION,
        CoreError::InvalidChart { .. } => codes::INVALID_CHART,
        CoreError::NotAggregatable { .. } => codes::NOT_AGGREGATABLE,
        CoreError::InvalidValue { .. } => codes::INVALID_VALUE,
    }
}

/// `kaikei_policy::PolicyError` に対応するエラーコードを返す。
///
/// `PolicyError::Core` は中身の `CoreError` へ委譲する（`AppError::Core` と
/// 同じコードになる。AI から見て「どの層を経由したか」は分類軸ではない）。
///
/// `PolicyError` は `#[non_exhaustive]` を**意図的に付けていない**
/// （`kaikei-policy/src/error.rs` の doc）ため、ここも網羅 `match` にする。
pub fn policy_error_code(err: &PolicyError) -> &'static str {
    match err {
        PolicyError::Core(inner) => core_error_code(inner),
        PolicyError::NoApplicableRuleSet { .. } => codes::NO_APPLICABLE_RULE_SET,
        PolicyError::UnknownTaxCategory { .. } => codes::UNKNOWN_TAX_CATEGORY,
        PolicyError::TaxCategoryNotApplicable { .. } => codes::TAX_CATEGORY_NOT_APPLICABLE,
        PolicyError::UnknownCounterparty { .. } => codes::UNKNOWN_COUNTERPARTY,
        PolicyError::QualifiedInvoiceUnverified { .. } => codes::QUALIFIED_INVOICE_UNVERIFIED,
        PolicyError::InvalidPolicyData { .. } => codes::INVALID_POLICY_DATA,
        PolicyError::Unsupported { .. } => codes::POLICY_UNSUPPORTED,
    }
}

/// 永続化層（[`crate::ports::Store`] の実装。`kaikei-store` 等）が返すエラー。
///
/// バリアントを分けるのは、呼び出し側（ユースケース）が「次に何をすべきか」を
/// 判断できるようにするため。例えば [`RepoError::AppendOnlyViolation`] を
/// 受け取ったユースケースは「訂正は逆仕訳（`reverse`）で行ってください」と
/// 案内できるが、単一の `Backend` バリアントに潰れているとその案内を
/// 組み立てられない。
///
/// # `reason` をそのまま外部に出さない
///
/// 各バリアントの `reason` は**診断のための文字列**であり、実装
/// （`kaikei-store` の `sqlstate::map_sqlstate`）は
/// `format!("...: {message}")` として **DB が返したメッセージをそのまま
/// 埋めている**（接続文字列・ロール名・制約定義が混じりうる）。
/// 外部への応答には `Display` ではなく [`RepoError::public_message`] を使う。
#[derive(Debug, thiserror::Error)]
pub enum RepoError {
    /// 指定した対象が永続化層に存在しない。
    #[error("見つかりません: {reason}")]
    NotFound {
        /// 見つからなかった対象の説明。
        reason: String,
    },

    /// append-only の制約（DB 権限の REVOKE、または所有者もバイパスできない
    /// 最後の砦のトリガ）に反する操作をしようとした。帳簿の訂正は逆仕訳
    /// （`JournalEntry::reverse`）のみが許される（`CLAUDE.md` §2）。
    #[error("この操作は許可されていません（帳簿への訂正は逆仕訳のみです）: {reason}")]
    AppendOnlyViolation {
        /// 拒否の詳細（どの操作がどう拒否されたか）。
        reason: String,
    },

    /// 一意制約違反（重複データ）。例えば同じ仕訳IDや `(fiscal_year,
    /// entry_no)` の組を持つ仕訳を重ねて挿入しようとした場合に返す
    /// （[`crate::ports::JournalRepo::insert_entry`] の `# Errors` を参照）。
    #[error("既に存在します: {reason}")]
    Conflict {
        /// 重複の詳細。
        reason: String,
    },

    /// 保存されているデータが不正（永続化層からの復元処理の直前に行う
    /// 再検証で検出）。panic させず、この形で呼び出し側に返す。
    #[error("保存データが不正です: {reason}")]
    Corrupt {
        /// 不正の詳細。
        reason: String,
    },

    /// 金額・仕訳番号等が変換先の型で表現できる範囲を超えている
    /// （例: `i128` → `i64`、`u32` → `i32` の変換失敗）。
    #[error("値が範囲外です: {reason}")]
    OutOfRange {
        /// 範囲外の詳細。
        reason: String,
    },

    /// 現在の実装ではサポートしていない操作（例: 逆仕訳への証憑紐付け）。
    #[error("この操作はサポートされていません: {reason}")]
    Unsupported {
        /// サポートされない理由。
        reason: String,
    },

    /// 上記のいずれにも分類できない永続化層の失敗（接続断等）。
    #[error("永続化層でエラーが発生しました: {reason}")]
    Backend {
        /// 失敗の詳細。
        reason: String,
    },
}

impl RepoError {
    /// このエラーに対応するエラーコード（[`codes`]）を返す。
    ///
    /// `RepoError` は `#[non_exhaustive]` では**ない**（[`AppError`] とは
    /// 逆の判断。この enum を構築するのは `kaikei-store` 等の実装側であり、
    /// バリアント追加は写像表・SQLSTATE 表の同時更新を伴うべきなので、
    /// 網羅 `match` がコンパイルエラーで知らせる方を採る）。
    /// したがってここにワイルドカードの受け皿は置かない。
    pub fn code(&self) -> &'static str {
        match self {
            RepoError::NotFound { .. } => codes::NOT_FOUND,
            RepoError::AppendOnlyViolation { .. } => codes::APPEND_ONLY_VIOLATION,
            RepoError::Conflict { .. } => codes::CONFLICT,
            RepoError::Corrupt { .. } => codes::CORRUPT,
            RepoError::OutOfRange { .. } => codes::OUT_OF_RANGE,
            RepoError::Unsupported { .. } => codes::REPO_UNSUPPORTED,
            RepoError::Backend { .. } => codes::BACKEND,
        }
    }

    /// **外部（MCP / HTTP の応答、`audit_log.output`）に出してよい本文**を返す。
    ///
    /// `Display`（`to_string()`）は `reason` をそのまま含むため、
    /// **下位層が返した生のメッセージが外に漏れうる**
    /// （`kaikei-store` の `sqlstate::map_sqlstate` は DB のメッセージを
    /// `reason` に埋め込む）。`docs/07-mcp-server.md` §9 が
    /// 「接続文字列・認証情報を含みうる下位層のエラー本文をそのまま転記しない」
    /// と定めているのはこの経路のこと。
    ///
    /// | バリアント | 扱い |
    /// |---|---|
    /// | `NotFound` | `Display` をそのまま（`reason` は app 層が組み立てる。仕訳IDの UUID 正準表記を含む） |
    /// | `Unsupported` | `Display` をそのまま（未実装の説明であり DB 由来ではない） |
    /// | `AppendOnlyViolation` / `Conflict` / `OutOfRange` / `Corrupt` / `Backend` | **正規化**（`reason` を出さず、分類ごとの汎用文言 + 次の手） |
    ///
    /// 正規化した側でも「次の手が分かる文言」は保つ（`CLAUDE.md` §11）。
    /// 消えた詳細は `Display` 側に残っているので、**サーバのログには
    /// `Display` を、応答にはこちらを**出すこと。
    pub fn public_message(&self) -> String {
        match self {
            // `reason` を app 層自身が組み立てるバリアント。
            // `NotFound` の `reason` は `reverse_entry::execute` が
            // 仕訳IDの UUID 正準表記を入れて作る（`docs/07-mcp-server.md` §3）。
            // ここで潰すと AI が「どのIDが無かったのか」を失う。
            RepoError::NotFound { .. } | RepoError::Unsupported { .. } => self.to_string(),

            RepoError::AppendOnlyViolation { .. } => {
                "この操作は許可されていません。帳簿（仕訳）は追記のみで、\
                 更新・削除はできません。訂正は逆仕訳（reverse_journal_entry）で\
                 行ってください"
                    .to_string()
            }
            RepoError::Conflict { .. } => "既に存在するデータと重複するため保存できませんでした。\
                 同じ仕訳を二重に登録しようとしていないか確認してください"
                .to_string(),
            RepoError::OutOfRange { .. } => {
                "値が保存可能な範囲を超えています。金額の桁数を確認してください".to_string()
            }
            RepoError::Corrupt { .. } => {
                "保存データの整合性検証に失敗しました。この操作は完了していません。\
                 入力を変えても解消しません。サーバのログを添えて管理者に連絡してください"
                    .to_string()
            }
            RepoError::Backend { .. } => {
                "永続化層でエラーが発生しました。この操作は完了していません。\
                 入力の問題ではないため、時間をおいて再試行するか、\
                 サーバのログを添えて管理者に連絡してください"
                    .to_string()
            }
        }
    }
}

/// ユースケースが失敗したときに返すエラー。
///
/// [`RepoError`] / [`PolicyError`] / [`CoreError`] をそのまま伝播できるように
/// `#[from]` を用意する。
///
/// `#[non_exhaustive]` を付ける。`kaikei-policy::PolicyError` が意図的に
/// `#[non_exhaustive]` を**付けない**選択をしているのとは逆の判断である
/// （`kaikei-policy/src/error.rs` の doc を参照）。`PolicyError` の消費者は
/// `kaikei-app` の中だけ（同一ワークスペース内で足並みを揃えて更新できる）
/// だが、`AppError` の消費者は `kaikei-api` / `kaikei-mcp` のようなさらに
/// 下流の crate になる。バリアント追加のたびにそれらの網羅的 `match` が
/// 壊れるより、`_ => {}` の一手で追従できる方が実用上安全と判断した
/// （後から `#[non_exhaustive]` を付けるのは破壊的変更になるため、最初から
/// 付けておく）。
///
/// **`#[non_exhaustive]` は crate の外にしか効かない。** したがって
/// `kaikei-app` 自身の [`AppError::code`] / [`AppError::public_message`] は
/// ワイルドカードを持たない網羅 `match` であり、バリアントを足すと
/// **この crate のビルドが壊れて**割り当て漏れが露見する。
/// 下流の追従しやすさと、定義元での取りこぼしの検出は両立する。
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AppError {
    /// 永続化層のエラー。
    #[error(transparent)]
    Repo(#[from] RepoError),

    /// `kaikei-policy` の trait 実装（`TaxPolicy` 等）が返したエラー。
    #[error(transparent)]
    Policy(#[from] PolicyError),

    /// `kaikei-core` の不変条件違反。
    #[error(transparent)]
    Core(#[from] CoreError),

    /// 既に赤伝（逆仕訳）済みの仕訳を再度取り消そうとした。
    ///
    /// 二重取消は既定で拒否する（誤操作・AI の暴走が帳簿の残高を静かに
    /// 壊すことを防ぐ多層防御の一つ）。許可する運用は
    /// `ReverseEntryInput::allow_double_reversal` を明示した場合のみ。
    ///
    /// # なぜ `reversal_id` を持つのか
    ///
    /// 呼び出し元（MCP / API）が仕訳を指すのに使うのは **UUID**（`original_id`）
    /// であって通し番号ではない。既存の赤伝を番号だけで返すと、AI は
    /// 「その赤伝を見る」ために `entry_no` から `EntryId` を引く手段を持たない
    /// （`JournalRepo` は `find_entry(EntryId)` しか無く、番号からの検索は
    /// 存在しない）。[`crate::ports::JournalRepo::find_reversal_of`] は
    /// 元々 `(EntryId, EntryNumber)` を返しており、`EntryId` を捨てていたのは
    /// このバリアントが受け皿を持たなかったからでしかない。
    ///
    /// 番号（`entry_no` / `reversal_no`）は人間が帳簿を目で追うための表示で、
    /// **両方を持つ**（どちらか一方に寄せない）。
    #[error(
        "仕訳 {} は既に取消（逆仕訳 {}、仕訳ID {}）済みです。\
         その赤伝の内容は仕訳ID {} で確認できます。\
         それでも二重取消が必要な場合は allow_double_reversal を指定してください",
        entry_no.as_u32(),
        reversal_no.as_u32(),
        entry_id_to_uuid_string(*reversal_id),
        entry_id_to_uuid_string(*reversal_id)
    )]
    AlreadyReversed {
        /// 取り消そうとした仕訳の番号。
        entry_no: EntryNumber,
        /// 既存の逆仕訳の番号（人間向けの表示）。
        reversal_no: EntryNumber,
        /// 既存の逆仕訳の仕訳ID。応答には **UUID の正準表記**で載せること
        /// （[`crate::id::entry_id_to_uuid_string`]）。
        reversal_id: EntryId,
    },

    /// 仕訳IDとして渡された文字列が **UUID の正準表記として解釈できない**。
    ///
    /// 「その仕訳が存在しない」（`RepoError::NotFound`）とは**別のエラー**
    /// である。AI が取るべき次の手が違う（前者はIDを調べ直す、後者は
    /// 表記そのものを直す）ため、コードも分けている
    /// （[`codes::INVALID_ENTRY_ID`] / [`codes::NOT_FOUND`]）。
    ///
    /// 生成元は [`crate::id::entry_id_from_uuid_string`]。
    #[error(
        "仕訳IDの形式が不正です: \"{input}\"。\
         仕訳IDは UUID の正準表記（ハイフン付き36文字。\
         例: 0192a7b3-1234-7abc-8def-0123456789ab）で指定してください"
    )]
    InvalidEntryId {
        /// 受け取った文字列（長すぎる場合は先頭のみ。
        /// [`crate::id::entry_id_from_uuid_string`] が切り詰める）。
        input: String,
    },

    /// 訂正理由（`reverse_entry` の `reason`）が空文字、または空白のみだった。
    ///
    /// 下位層はいずれもこれを通す（`kaikei_core::JournalEntry::reverse` は
    /// 受け取った文字列をそのまま `reverse_reason` に代入し、DB の
    /// `CHECK ((reverses IS NULL) = (reverse_reason IS NULL))` は NULL の
    /// 一致しか見ない）。呼び出し元ごとに検証を書くと必ずどこかが抜けるため、
    /// **ユースケース層**（[`crate::usecase::reverse_entry::execute`]）で
    /// 弾く（`DECISIONS.md` D-074）。
    #[error(
        "訂正理由（reason）が空です。何をなぜ訂正するのかを記入してください\
         （例: 「請求金額の誤り（税率の適用誤り）」）。\
         空白のみの文字列は理由として扱いません"
    )]
    EmptyReverseReason,

    /// 試算表の検算に失敗した（借方合計 ≠ 貸方合計）。正しく記帳された
    /// データからは発生しない。データ破損、または実装のバグを示す。
    #[error(
        "試算表の貸借が一致しません: 借方 {debit} / 貸方 {credit}。\
         データが破損している可能性があります。管理者に連絡してください"
    )]
    Inconsistent {
        /// 借方合計（表示用文字列）。
        debit: String,
        /// 貸方合計（表示用文字列）。
        credit: String,
    },

    /// 上記に分類できない業務ルール違反。次の手が分かる文言にすること
    /// （`CLAUDE.md` §11）。
    #[error("{reason}")]
    Rejected {
        /// 拒否の理由と、可能であれば次に取るべき手。
        reason: String,
    },
}

impl AppError {
    /// このエラーに対応するエラーコード（[`codes`]）を返す。
    ///
    /// presentation 層（`kaikei-mcp`）は応答の `error` フィールドと
    /// `audit_log.error_code` にこの値を載せる。**メッセージ本文
    /// （`Display`）とは別物**であり、文言を改善してもこの値は変わらない
    /// （[`codes`] の doc を参照）。
    ///
    /// [`AppError::Repo`] / [`AppError::Policy`] / [`AppError::Core`] は
    /// 中身のエラーへ委譲する（`AppError::Core(CoreError::Unbalanced)` は
    /// `"app_core"` ではなく `"unbalanced"` になる。AI が知りたいのは
    /// 「何が起きたか」であって「どの層を経由したか」ではない）。
    ///
    /// # ★ワイルドカードの腕を置かない★
    ///
    /// この `match` には `_ => codes::INTERNAL` の受け皿を**置かない**
    /// （1巡目では置いていた。`DECISIONS.md` D-072 の訂正注記）。
    ///
    /// - 受け皿があると、`AppError` にバリアントを足してもこの関数の
    ///   コンパイルは壊れず、そのバリアントは黙って `"internal"` になる。
    ///   「手で維持する一覧は必ず腐る」（`PROGRESS.md` Phase 1 の教訓6）の
    ///   典型であり、それを別のテスト関数（重複した網羅 `match`）で
    ///   見張るのは**同じ一覧を2つ手で維持する**ことにほかならない。
    /// - 受け皿を消すと `#[allow(unreachable_patterns)]` も不要になる
    ///   （lint は受け皿の腕自身が作り出していた問題だった）。
    /// - `#[non_exhaustive]` は crate の**外**にしか効かないので、
    ///   この判断は下流の網羅性要件を何も変えない。下流の `match` には
    ///   引き続きワイルドカードが必須で、そこで使う既定値が
    ///   [`codes::INTERNAL`] である。実際にそうなることは
    ///   `tests/contract_from_downstream.rs` が外部 crate として検証する。
    pub fn code(&self) -> &'static str {
        match self {
            AppError::Repo(inner) => inner.code(),
            AppError::Policy(inner) => policy_error_code(inner),
            AppError::Core(inner) => core_error_code(inner),
            AppError::AlreadyReversed { .. } => codes::ALREADY_REVERSED,
            AppError::EmptyReverseReason => codes::EMPTY_REVERSE_REASON,
            AppError::InvalidEntryId { .. } => codes::INVALID_ENTRY_ID,
            AppError::Inconsistent { .. } => codes::INCONSISTENT,
            AppError::Rejected { .. } => codes::REJECTED,
        }
    }

    /// **外部（MCP / HTTP の応答、`audit_log.output`）に出してよい本文**を返す。
    ///
    /// `docs/07-mcp-server.md` §3 の `message` フィールドに載せるのはこの値
    /// （`Display` ではない）。両者の使い分けは本モジュール doc の
    /// 「本文には2つの入口がある」を参照。
    ///
    /// - [`AppError::Repo`] は [`RepoError::public_message`] に委譲する
    ///   （下位層の生メッセージを含みうるバリアントだけが正規化される）。
    /// - [`AppError::Policy`] / [`AppError::Core`] と `AppError` 自身の
    ///   バリアントは、文言をこのリポジトリが書いているので `Display` を
    ///   そのまま返す。**言い換えない**（`CLAUDE.md` §10。policy が組み立てた
    ///   文言を上位層で税務判断に踏み込んだ表現へ書き換えないため）。
    ///
    /// この `match` にもワイルドカードを置かない（[`AppError::code`] と同じ理由）。
    pub fn public_message(&self) -> String {
        match self {
            AppError::Repo(inner) => inner.public_message(),
            AppError::Policy(_)
            | AppError::Core(_)
            | AppError::AlreadyReversed { .. }
            | AppError::EmptyReverseReason
            | AppError::InvalidEntryId { .. }
            | AppError::Inconsistent { .. }
            | AppError::Rejected { .. } => self.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaikei_core::{AccountType, TagValueType};

    // 1巡目にあった `exhaustive_app_error_code`（ワイルドカードを持たない
    // 網羅 `match` のコピー）は削除した。[`AppError::code`] 自身が
    // ワイルドカードを持たなくなり、バリアント追加は `cargo build` を壊す。
    // 同じ一覧を2つ手で維持する方が腐りやすい（`DECISIONS.md` D-072 の訂正注記）。

    fn all_core_errors() -> Vec<CoreError> {
        vec![
            CoreError::Unbalanced {
                debit: "110,000".to_string(),
                credit: "100,000".to_string(),
                diff: "10,000".to_string(),
            },
            CoreError::TooFewLines { found: 1 },
            CoreError::UnknownAccount {
                code: "999".to_string(),
            },
            CoreError::NotPostable {
                code: "100".to_string(),
            },
            CoreError::CurrencyMismatch {
                a: "JPY".to_string(),
                b: "USD".to_string(),
            },
            CoreError::InvalidAmount {
                reason: "桁数超過".to_string(),
            },
            CoreError::UnknownTagKey {
                key: "unknown".to_string(),
            },
            CoreError::TagTypeMismatch {
                key: "business_ratio".to_string(),
                expected: TagValueType::Decimal,
            },
            CoreError::MissingRequiredTag {
                key: "tax_category".to_string(),
                account_type: AccountType::Revenue,
            },
            CoreError::DateOutOfFiscalYear {
                date: "2026-01-01".to_string(),
                fy: 2025,
                start: "2025-01-01".to_string(),
                end: "2025-12-31".to_string(),
            },
            CoreError::PeriodClosed {
                date: "2026-01-15".to_string(),
            },
            CoreError::EmptyDescription,
            CoreError::InvalidChart {
                reason: "親科目が存在しません".to_string(),
            },
            CoreError::NotAggregatable {
                key: "memo".to_string(),
            },
            CoreError::InvalidValue {
                reason: "形式が不正です".to_string(),
            },
        ]
    }

    fn all_policy_errors() -> Vec<PolicyError> {
        vec![
            PolicyError::Core(CoreError::EmptyDescription),
            PolicyError::NoApplicableRuleSet {
                as_of: "2030-01-01".to_string(),
            },
            PolicyError::UnknownTaxCategory {
                code: "SALES_99".to_string(),
                as_of: "2026-04-01".to_string(),
                available: "SALES_10".to_string(),
            },
            PolicyError::TaxCategoryNotApplicable {
                account: "100".to_string(),
                code: "SALES_10".to_string(),
                reason: "資産科目には適用できません".to_string(),
            },
            PolicyError::UnknownCounterparty {
                code: "CP9999".to_string(),
            },
            PolicyError::QualifiedInvoiceUnverified {
                code: "CP0001".to_string(),
                counterparty: "A社".to_string(),
            },
            PolicyError::InvalidPolicyData {
                reason: "マスタが不正です".to_string(),
            },
            PolicyError::Unsupported {
                reason: "簡易課税は未実装です".to_string(),
            },
        ]
    }

    fn all_repo_errors() -> Vec<RepoError> {
        vec![
            RepoError::NotFound {
                reason: "見つかりません".to_string(),
            },
            RepoError::AppendOnlyViolation {
                reason: "UPDATE は拒否されました".to_string(),
            },
            RepoError::Conflict {
                reason: "重複しています".to_string(),
            },
            RepoError::Corrupt {
                reason: "行が壊れています".to_string(),
            },
            RepoError::OutOfRange {
                reason: "i64 の範囲を超えています".to_string(),
            },
            RepoError::Unsupported {
                reason: "証憑の紐付けは未実装です".to_string(),
            },
            RepoError::Backend {
                reason: "接続が切れました".to_string(),
            },
        ]
    }

    /// `AppError` 自身のバリアント（委譲するものを除く）の代表値。
    fn own_app_errors() -> Vec<AppError> {
        vec![
            AppError::AlreadyReversed {
                entry_no: EntryNumber::new(42),
                reversal_no: EntryNumber::new(43),
                reversal_id: crate::id::entry_id_from_uuid(
                    uuid::Uuid::parse_str("0192b1c4-1234-7abc-8def-0123456789ab").unwrap(),
                ),
            },
            AppError::EmptyReverseReason,
            AppError::InvalidEntryId {
                input: "42".to_string(),
            },
            AppError::Inconsistent {
                debit: "110,000".to_string(),
                credit: "100,000".to_string(),
            },
            AppError::Rejected {
                reason: "業務ルール違反".to_string(),
            },
        ]
    }

    // EC-1: CoreError の全15バリアントからコードが引け、すべて異なる。
    #[test]
    fn every_core_error_variant_has_a_distinct_code() {
        let errors = all_core_errors();
        assert_eq!(errors.len(), 15, "CoreError は15バリアント");
        let mut codes: Vec<&str> = errors.iter().map(core_error_code).collect();
        codes.sort_unstable();
        let distinct = codes.len();
        codes.dedup();
        assert_eq!(codes.len(), distinct, "コードが重複しています: {codes:?}");
        assert!(codes.iter().all(|c| *c != codes::INTERNAL));
    }

    // EC-2: PolicyError の全8バリアントからコードが引ける。
    // `Core` は委譲するため、それ自身の固有コードは持たない。
    #[test]
    fn every_policy_error_variant_has_a_code() {
        let errors = all_policy_errors();
        assert_eq!(errors.len(), 8, "PolicyError は8バリアント");
        assert_eq!(
            policy_error_code(&PolicyError::Core(CoreError::EmptyDescription)),
            codes::EMPTY_DESCRIPTION,
            "PolicyError::Core は中身の CoreError へ委譲する"
        );
        assert!(errors
            .iter()
            .all(|e| policy_error_code(e) != codes::INTERNAL));
    }

    // EC-3: RepoError の全7バリアントからコードが引け、すべて異なる。
    #[test]
    fn every_repo_error_variant_has_a_distinct_code() {
        let errors = all_repo_errors();
        assert_eq!(errors.len(), 7, "RepoError は7バリアント");
        let mut codes: Vec<&str> = errors.iter().map(RepoError::code).collect();
        codes.sort_unstable();
        let distinct = codes.len();
        codes.dedup();
        assert_eq!(codes.len(), distinct, "コードが重複しています: {codes:?}");
        assert!(codes.iter().all(|c| *c != codes::INTERNAL));
    }

    // EC-4: AppError 自身のバリアントからコードが引ける。
    #[test]
    fn every_own_app_error_variant_has_a_code() {
        for err in own_app_errors() {
            assert_ne!(err.code(), codes::INTERNAL, "受け皿に落ちています: {err:?}");
        }
    }

    // EC-5: 委譲（Repo / Policy / Core）が中身のコードをそのまま返す。
    #[test]
    fn app_error_delegates_to_the_inner_error_code() {
        for inner in all_core_errors() {
            let expected = core_error_code(&inner);
            assert_eq!(AppError::Core(inner).code(), expected);
        }
        for inner in all_policy_errors() {
            let expected = policy_error_code(&inner);
            assert_eq!(AppError::Policy(inner).code(), expected);
        }
        for inner in all_repo_errors() {
            let expected = inner.code();
            assert_eq!(AppError::Repo(inner).code(), expected);
        }
    }

    // EC-6: **下流用**の既定コードの値が固定されていること。
    //
    // `AppError::code` はワイルドカードの腕を持たないため、この値が
    // `kaikei-app` の中から返ることは無い（EC-4 が「受け皿に落ちていない」
    // ではなく「全バリアントが固有のコードを持つ」を検査するのはそのため）。
    // この定数が要るのは下流（`kaikei-mcp` / `kaikei-api`）の `match` であり、
    // 実際にそこで使われることは `tests/contract_from_downstream.rs` が
    // 外部 crate として検証する。
    #[test]
    fn the_downstream_fallback_code_is_internal() {
        assert_eq!(codes::INTERNAL, "internal");
    }

    // EC-7: 監査ログの fail-closed 用コードが `rejected` と衝突しない。
    //
    // 「入力を直せば通る」（`Rejected`。例: 集計期間の開始日が終了日より後）と
    // 「サーバ都合で今は実行できない」（監査ログに記録できなかった）は
    // AI が取るべき次の手が違うため、同じコードに潰さない
    // （`docs/07-mcp-server.md` §9）。
    #[test]
    fn the_audit_fail_closed_code_is_distinct_from_rejected() {
        assert_eq!(codes::AUDIT_LOG_UNAVAILABLE, "audit_log_unavailable");
        assert_ne!(codes::AUDIT_LOG_UNAVAILABLE, codes::REJECTED);
        // `AppError` のどのバリアントもこのコードを返さない
        // （fail-closed の判定はユースケースに到達しない位置で起きる）。
        let mut errors = own_app_errors();
        errors.extend(all_core_errors().into_iter().map(AppError::Core));
        errors.extend(all_policy_errors().into_iter().map(AppError::Policy));
        errors.extend(all_repo_errors().into_iter().map(AppError::Repo));
        for err in errors {
            assert_ne!(err.code(), codes::AUDIT_LOG_UNAVAILABLE);
        }
    }

    // EC-8: コードは snake_case の安定した識別子である。
    #[test]
    fn codes_are_snake_case_ascii_identifiers() {
        let mut errors = own_app_errors();
        errors.extend(all_core_errors().into_iter().map(AppError::Core));
        errors.extend(all_policy_errors().into_iter().map(AppError::Policy));
        errors.extend(all_repo_errors().into_iter().map(AppError::Repo));
        for err in errors {
            let code = err.code();
            assert!(!code.is_empty(), "コードが空です");
            assert!(
                code.chars()
                    .all(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit()),
                "snake_case ではありません: {code}"
            );
        }
    }

    // EC-9: コードとメッセージは別物である（同じコードでもメッセージは
    // 入力によって変わる。逆に、メッセージを変えてもコードは変わらない）。
    #[test]
    fn the_code_is_independent_of_the_message() {
        let a = AppError::Core(CoreError::UnknownAccount {
            code: "999".to_string(),
        });
        let b = AppError::Core(CoreError::UnknownAccount {
            code: "888".to_string(),
        });
        assert_eq!(a.code(), b.code());
        assert_ne!(a.to_string(), b.to_string());
    }

    // EC-10: 層をまたいで同名のバリアント（`Unsupported`）は別コードになる。
    #[test]
    fn policy_and_repo_unsupported_do_not_collapse_into_one_code() {
        let policy = AppError::Policy(PolicyError::Unsupported {
            reason: "簡易課税は未実装です".to_string(),
        });
        let repo = AppError::Repo(RepoError::Unsupported {
            reason: "証憑の紐付けは未実装です".to_string(),
        });
        assert_ne!(policy.code(), repo.code());
    }

    // EC-11（PR-B 2巡目）: `AlreadyReversed` は既存赤伝の仕訳IDを
    // **UUID の正準表記**でメッセージに含める。番号だけだと、AI は
    // その赤伝を `find_entry(EntryId)` で引き直せない。
    #[test]
    fn already_reversed_reports_the_existing_reversal_id_as_a_canonical_uuid() {
        let reversal_id = crate::id::entry_id_from_uuid(
            uuid::Uuid::parse_str("0192b1c4-1234-7abc-8def-0123456789ab").unwrap(),
        );
        let err = AppError::AlreadyReversed {
            entry_no: EntryNumber::new(42),
            reversal_no: EntryNumber::new(43),
            reversal_id,
        };
        let message = err.to_string();
        assert!(
            message.contains("0192b1c4-1234-7abc-8def-0123456789ab"),
            "{message}"
        );
        // 10進表記は混ざらない。
        assert!(
            !message.contains(&reversal_id.as_u128().to_string()),
            "{message}"
        );
        // 人間向けの番号も残っている。
        assert!(
            message.contains("42") && message.contains("43"),
            "{message}"
        );
        assert_eq!(err.code(), codes::ALREADY_REVERSED);
    }

    // EC-12（PR-B 2巡目）: `InvalidEntryId` は `NotFound` と別コードになる。
    #[test]
    fn invalid_entry_id_is_not_confused_with_not_found() {
        let malformed = AppError::InvalidEntryId {
            input: "42".to_string(),
        };
        let missing = AppError::Repo(RepoError::NotFound {
            reason: "仕訳が見つかりません".to_string(),
        });
        assert_eq!(malformed.code(), codes::INVALID_ENTRY_ID);
        assert_eq!(missing.code(), codes::NOT_FOUND);
        assert_ne!(malformed.code(), missing.code());
        // 次の手（正しい表記の例）が含まれる（`CLAUDE.md` §11）。
        assert!(malformed.to_string().contains("36"), "{malformed}");
    }

    // EC-13（PR-B 2巡目）: 下位層の生メッセージを含みうるバリアントの
    // `public_message` は、その生メッセージを含まない。
    //
    // `kaikei-store::sqlstate::map_sqlstate` が実際に組み立てる形
    // （`format!("...: {message}")`）を模した `reason` を与えて確認する。
    #[test]
    fn public_message_does_not_leak_the_backend_reason() {
        const SECRET: &str = "postgres://kaikei_app:s3cret@db.internal:5432/kaikei";
        let leaky = [
            RepoError::Backend {
                reason: format!("未分類のデータベースエラーです（SQLSTATE 08006）: {SECRET}"),
            },
            RepoError::Corrupt {
                reason: format!("保存しようとしたデータが制約に違反しています: {SECRET}"),
            },
            RepoError::AppendOnlyViolation {
                reason: format!("権限エラーです（SQLSTATE 42501）: {SECRET}"),
            },
            RepoError::Conflict {
                reason: format!("一意制約違反です（SQLSTATE 23505）: {SECRET}"),
            },
            RepoError::OutOfRange {
                reason: format!("数値が範囲を超えました（SQLSTATE 22003）: {SECRET}"),
            },
        ];
        for err in leaky {
            // 診断用の `Display` には残っている（サーバのログ向け）。
            assert!(err.to_string().contains(SECRET), "{err:?}");
            // 外部に出す本文には残らない。
            assert!(
                !err.public_message().contains(SECRET),
                "生メッセージが漏れています: {}",
                err.public_message()
            );
            assert!(
                !AppError::Repo(err).public_message().contains(SECRET),
                "AppError 経由で漏れています"
            );
        }
    }

    // EC-14（PR-B 2巡目）: 正規化した本文も「次の手が分かる」文言を保つ。
    #[test]
    fn normalized_public_messages_still_tell_the_caller_what_to_do() {
        let append_only = RepoError::AppendOnlyViolation {
            reason: "permission denied for table journal_entries".to_string(),
        };
        assert!(
            append_only.public_message().contains("逆仕訳"),
            "{}",
            append_only.public_message()
        );

        let backend = RepoError::Backend {
            reason: "connection refused".to_string(),
        };
        // 「入力を直せば通る」と誤解させないこと（再試行・管理者連絡へ誘導する）。
        assert!(
            backend.public_message().contains("入力"),
            "{}",
            backend.public_message()
        );
    }

    // EC-15（PR-B 2巡目）: ドメインのエラーは `public_message` でも
    // `Display` と同じ本文を返す（言い換えない。`CLAUDE.md` §10）。
    //
    // 特に `NotFound` は app 層が組み立てる `reason`（仕訳IDの UUID 正準表記を
    // 含む）を落とさないこと。ここを潰すと `docs/07-mcp-server.md` §3 の
    // 「仕訳IDは UUID の正準表記で示す」が応答から消える。
    #[test]
    fn domain_errors_keep_their_display_text_in_the_public_message() {
        let not_found = RepoError::NotFound {
            reason: "仕訳が見つかりません（仕訳ID: 0192a7b3-1234-7abc-8def-0123456789ab）"
                .to_string(),
        };
        assert_eq!(not_found.public_message(), not_found.to_string());
        assert!(not_found
            .public_message()
            .contains("0192a7b3-1234-7abc-8def-0123456789ab"));

        let unsupported = RepoError::Unsupported {
            reason: "証憑の紐付けは未実装です".to_string(),
        };
        assert_eq!(unsupported.public_message(), unsupported.to_string());

        let mut domain = own_app_errors();
        domain.extend(all_core_errors().into_iter().map(AppError::Core));
        domain.extend(all_policy_errors().into_iter().map(AppError::Policy));
        for err in domain {
            assert_eq!(err.public_message(), err.to_string(), "{err:?}");
        }
    }

    // EC-16（PR-B 2巡目）: `public_message` は空文字を返さない
    // （空の本文は AI にとって「エラーの理由が無い」と同じ）。
    #[test]
    fn public_message_is_never_empty() {
        let mut errors = own_app_errors();
        errors.extend(all_core_errors().into_iter().map(AppError::Core));
        errors.extend(all_policy_errors().into_iter().map(AppError::Policy));
        errors.extend(all_repo_errors().into_iter().map(AppError::Repo));
        for err in errors {
            assert!(!err.public_message().trim().is_empty(), "{err:?}");
        }
    }
}
