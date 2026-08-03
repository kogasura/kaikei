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

use kaikei_core::{CoreError, EntryNumber};
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
    /// `AppError::Inconsistent`（試算表の検算失敗）。
    pub const INCONSISTENT: &str = "inconsistent";
    /// `AppError::Rejected`（上記に分類できない業務ルール違反）。
    pub const REJECTED: &str = "rejected";

    // ---- 受け皿 ----

    /// 未知のバリアントに対する既定コード。
    ///
    /// [`super::AppError`] は `#[non_exhaustive]` なので、将来この crate に
    /// 追加されたバリアントが対応表の更新より先に下流へ届くことがありうる。
    /// そのとき実装者が場当たりのコードを発明しないよう、既定を1つに決めておく
    /// （`docs/07-mcp-server.md` §6）。
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
    /// 壊すことを防ぐ多層防御の一つ）。許可する運用（`allow_double_reversal`
    /// をユースケース入力に明示した場合のみ許可）は、その入力型を持つ
    /// ユースケース本体（後続の PR）が実装する。
    #[error(
        "仕訳 {} は既に取消（逆仕訳 {}）済みです。\
         二重取消を許可する場合は allow_double_reversal を指定してください",
        entry_no.as_u32(),
        reversal_no.as_u32()
    )]
    AlreadyReversed {
        /// 取り消そうとした仕訳の番号。
        entry_no: EntryNumber,
        /// 既存の逆仕訳の番号。
        reversal_no: EntryNumber,
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
    pub fn code(&self) -> &'static str {
        match self {
            AppError::Repo(inner) => inner.code(),
            AppError::Policy(inner) => policy_error_code(inner),
            AppError::Core(inner) => core_error_code(inner),
            AppError::AlreadyReversed { .. } => codes::ALREADY_REVERSED,
            AppError::EmptyReverseReason => codes::EMPTY_REVERSE_REASON,
            AppError::Inconsistent { .. } => codes::INCONSISTENT,
            AppError::Rejected { .. } => codes::REJECTED,
            // `#[non_exhaustive]` の受け皿。
            //
            // 定義元 crate（ここ）から見ると `AppError` は網羅済みなので、
            // この腕は**現時点では到達しない**。それでも置くのは、下流
            // （`kaikei-mcp` / `kaikei-api`）がバリアント追加のたびに
            // 場当たりのコードを発明することを防ぐため
            // （`docs/07-mcp-server.md` §6）。
            //
            // `#[allow(unreachable_patterns)]` は必須。付けないと
            // `cargo clippy -- -D warnings`（CI の `quality` ジョブ）が
            // 「unreachable pattern」で落ちる（実測確認済み）。
            //
            // この腕があるためバリアント追加時にこの関数のコンパイルは
            // 壊れない。割り当て漏れを機械的に検出するのは、下部
            // `tests::exhaustive_app_error_code`（ワイルドカードを持たない
            // 網羅 `match`）の役目である。
            #[allow(unreachable_patterns)]
            _ => codes::INTERNAL,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaikei_core::{AccountType, TagValueType};

    /// **ワイルドカードを持たない**網羅 `match`。
    ///
    /// `AppError` にバリアントを追加すると、[`AppError::code`] 側は受け皿の
    /// `_ =>` があるためコンパイルが通ってしまう（そのバリアントは黙って
    /// `"internal"` になる）。この関数が非網羅としてコンパイルエラーになる
    /// ことで、**同じ PR の中でコードを割り当てる**ことを強制する。
    /// `PROGRESS.md` Phase 1 の教訓6「手で維持する一覧は必ず腐る。構造か CI で
    /// 機械的に閉じられないか先に考える」への対応。
    fn exhaustive_app_error_code(err: &AppError) -> &'static str {
        match err {
            AppError::Repo(inner) => inner.code(),
            AppError::Policy(inner) => policy_error_code(inner),
            AppError::Core(inner) => core_error_code(inner),
            AppError::AlreadyReversed { .. } => codes::ALREADY_REVERSED,
            AppError::EmptyReverseReason => codes::EMPTY_REVERSE_REASON,
            AppError::Inconsistent { .. } => codes::INCONSISTENT,
            AppError::Rejected { .. } => codes::REJECTED,
        }
    }

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
            },
            AppError::EmptyReverseReason,
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

    // EC-6: 受け皿（`_ => internal`）の既定値が `"internal"` であること。
    //
    // 定義元 crate から未知のバリアントを構築することは原理的にできないため、
    // 「受け皿に落ちたときに何が返るか」を実行時に踏むテストは書けない。
    // 代わりに (1) 既定コードの値が固定されていること、(2) 現存する全バリアントが
    // 受け皿に落ちていないこと（EC-1〜EC-4）、(3) バリアント追加時に
    // `exhaustive_app_error_code` がコンパイルエラーになること、の3点で守る。
    #[test]
    fn the_fallback_code_is_internal() {
        assert_eq!(codes::INTERNAL, "internal");
    }

    // EC-7: 受け皿を持つ `AppError::code` と、ワイルドカードを持たない
    // 網羅 `match` の結果が一致する（＝現時点で受け皿は一度も使われていない）。
    #[test]
    fn code_agrees_with_the_wildcard_free_exhaustive_match() {
        let mut errors = own_app_errors();
        errors.extend(all_core_errors().into_iter().map(AppError::Core));
        errors.extend(all_policy_errors().into_iter().map(AppError::Policy));
        errors.extend(all_repo_errors().into_iter().map(AppError::Repo));
        for err in errors {
            assert_eq!(
                err.code(),
                exhaustive_app_error_code(&err),
                "受け皿が使われています（コードの割り当て漏れ）: {err:?}"
            );
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
}
