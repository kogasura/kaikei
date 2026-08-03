//! ★契約凍結点★ の検証: PR-B で確定した契約が、**下流の crate から見て**
//! そのまま使えることを確かめる。
//!
//! 統合テスト（`tests/*.rs`）は `kaikei-app` を外部 crate としてリンクするため、
//! ここでのコンパイル可否は `kaikei-mcp` / `kaikei-api` から見た可否と同じになる
//! （`#[cfg(test)]` は効かず、`pub` でないものは見えない）。
//! `PROGRESS.md` Phase 1 の教訓2「契約を凍結する前に、その契約を使う側の
//! コードを実際に書いてみる」に従う。
//!
//! ここで踏むのは次の点:
//!
//! 1. エラーコードの語彙（`codes` の定数と4つの入口）が外から引けること
//! 2. **`#[non_exhaustive]` の受け皿が実際に効くこと。** 下流の `match` では
//!    `_` の腕が必須であり、しかも「到達しない」と警告されない
//!    （定義元 crate 内では逆に警告されるため、この検証はここでしかできない）
//! 3. `BookSettings` を外から構築できること（帳簿通貨を含む）
//! 4. 書き込み系ユースケースの戻り値（`PostEntryOutput` / `ReverseEntryOutput`）
//!    のフィールドが外から読めること
//!
//! PR-B 2巡目で追加した点（1巡目の消費側が「書けない／間違った書き方が
//! 通ってしまう」と指摘した箇所）:
//!
//! 5. **応答の JSON をこの crate だけで組み立てられること**——金額の
//!    区切り無し文字列（`amount`）、列挙型の機械可読名（`side` /
//!    `account_type` / `severity`）、仕訳IDの UUID 表記（入力・出力の両方向）が
//!    すべて `kaikei-app` から取れる。**下流が `uuid` や独自の `match` を
//!    足さずに済むこと**そのものを検査する
//! 6. **エラー本文の外向きの入口**（`public_message`）が下位層の生メッセージを
//!    含まないこと
//! 7. **dry-run（`preview`）が `kaikei-app` にあること**——`hint.suggested_lines`
//!    を組み立てるのに MCP 層が `load_posting_context` / `TaxContext` /
//!    `sum_money` を自分で書かずに済む
//! 8. 失敗経路でも `PolicyNote` が届くこと（`PostEntryFailure`）

use async_trait::async_trait;
use kaikei_app::amount::{money_to_plain_string, strip_thousands_separators};
use kaikei_app::context::{BookSettings, FiscalYearRule};
use kaikei_app::currency::currency_from_code;
use kaikei_app::error::{codes, core_error_code, policy_error_code, AppError, RepoError};
use kaikei_app::id::{entry_id_from_uuid, entry_id_from_uuid_string, entry_id_to_uuid_string};
use kaikei_app::ports::{JournalRepo, NumberingRepo, Store, TrialBalanceQuery, TxScope};
use kaikei_app::testing::{InMemoryStore, SequentialIdGenerator};
use kaikei_app::tx::{with_tx, with_tx_err};
use kaikei_app::usecase::post_entry::{
    self, PostEntryFailure, PostEntryInput, PostEntryOutput, PreviewEntryOutput,
};
use kaikei_app::usecase::report::{self, ReportInput};
use kaikei_app::usecase::reverse_entry::{self, ReverseEntryInput, ReverseEntryOutput};
use kaikei_app::view::BalanceRowView;
use kaikei_app::wire::{
    account_type_code, account_type_from_code, fiscal_year_rule_code, fiscal_year_rule_from_code,
    note_severity_code, side_code, side_from_code,
};
use kaikei_app::{NoteSeverity, PolicyNote, TaxContext, TaxDerivation, TaxPolicy};
use kaikei_core::{
    AccountCode, AccountDef, AccountType, AccountingDate, ChartOfAccounts, CoreError, Currency,
    EntryId, FixedClock, JournalEntry, JournalLine, Money, NewEntry, PeriodGuard, PeriodStatus,
    Ratio, RoundMode, Side, TagKey, TagSchema, TagSet, Timestamp,
};
use kaikei_policy::testing::{FlatRateTaxPolicy, NoTaxPolicy};
use kaikei_policy::PolicyError;

// ---- 下流（kaikei-mcp）が書くであろうコードの雛形 ----

/// `kaikei-mcp` の `error.rs` が書く写像の最小形。
///
/// **`AppError` は `#[non_exhaustive]` なので、この `match` には `_` の腕が
/// 必須である**（外さるとコンパイルエラーになる）。しかも定義元 crate と違って
/// 「到達しないパターン」の警告は出ない。これが受け皿を置いている理由そのもの:
/// `kaikei-app` にバリアントが増えても下流はコンパイルが通り続け、
/// 未知のバリアントは `internal` に落ちる（実装者が場当たりのコードを
/// 発明しない）。
fn downstream_error_code(err: &AppError) -> &'static str {
    match err {
        AppError::Core(inner) => core_error_code(inner),
        AppError::Policy(inner) => policy_error_code(inner),
        AppError::Repo(inner) => inner.code(),
        AppError::AlreadyReversed { .. } => codes::ALREADY_REVERSED,
        AppError::EmptyReverseReason => codes::EMPTY_REVERSE_REASON,
        // ここに `Inconsistent` / `Rejected` を**意図的に書かない**。
        // 下流が対応表の一部だけを実装した状態を再現し、残りが受け皿へ
        // 落ちることを実際に踏む。
        _ => codes::INTERNAL,
    }
}

// ---- フィクスチャ ----

fn chart() -> ChartOfAccounts {
    ChartOfAccounts::new(vec![
        AccountDef {
            code: AccountCode::parse("100").unwrap(),
            name: "現金".to_string(),
            account_type: AccountType::Asset,
            parent: None,
            postable: true,
        },
        AccountDef {
            code: AccountCode::parse("500").unwrap(),
            name: "売上高".to_string(),
            account_type: AccountType::Revenue,
            parent: None,
            postable: true,
        },
    ])
    .unwrap()
}

/// [`chart`] に仮受消費税(330・負債)を加えたもの。税額行の自動生成に使う。
fn chart_with_tax_account() -> ChartOfAccounts {
    ChartOfAccounts::new(vec![
        AccountDef {
            code: AccountCode::parse("100").unwrap(),
            name: "現金".to_string(),
            account_type: AccountType::Asset,
            parent: None,
            postable: true,
        },
        AccountDef {
            code: AccountCode::parse("500").unwrap(),
            name: "売上高".to_string(),
            account_type: AccountType::Revenue,
            parent: None,
            postable: true,
        },
        AccountDef {
            code: AccountCode::parse("330").unwrap(),
            name: "仮受消費税".to_string(),
            account_type: AccountType::Liability,
            parent: None,
            postable: true,
        },
    ])
    .unwrap()
}

/// 合成ルート（`kaikei-mcp` の `config.rs`）が組み立てる形。
/// 帳簿通貨は必須なので、省略するとコンパイルが通らない。
fn settings() -> BookSettings {
    BookSettings {
        fiscal_year_rule: FiscalYearRule::CalendarYear,
        book_currency: currency_from_code("JPY").unwrap(),
    }
}

fn clock() -> FixedClock {
    FixedClock(Timestamp::from_unix_nanos(0))
}

struct AllOpen;

impl PeriodGuard for AllOpen {
    fn status(&self, _date: AccountingDate) -> PeriodStatus {
        PeriodStatus::Open
    }
}

fn balanced_lines() -> Vec<JournalLine> {
    vec![
        JournalLine::new(
            AccountCode::parse("100").unwrap(),
            Side::Debit,
            Money::from_minor(1_000, Currency::JPY),
            TagSet::new(),
            None,
        )
        .unwrap(),
        JournalLine::new(
            AccountCode::parse("500").unwrap(),
            Side::Credit,
            Money::from_minor(1_000, Currency::JPY),
            TagSet::new(),
            None,
        )
        .unwrap(),
    ]
}

async fn seed_entry(store: &InMemoryStore, id: u128, date: AccountingDate) -> EntryId {
    let fy = kaikei_core::FiscalYear::calendar_year(date.year());
    let mut tx = store.begin().await.unwrap();
    let entry_no = tx.next_entry_no(fy.label()).await.unwrap();
    let entry = JournalEntry::new(
        NewEntry {
            id: EntryId::new(id),
            entry_no,
            entry_date: date,
            description: "元仕訳".to_string(),
            lines: balanced_lines(),
            document_refs: Vec::new(),
        },
        &fy,
        &chart(),
        &TagSchema::empty(),
        &AllOpen,
        &clock(),
    )
    .unwrap();
    tx.insert_entry(&entry).await.unwrap();
    tx.commit().await.unwrap();
    entry.id()
}

// ---- 1. エラーコードの語彙 ----

#[test]
fn error_codes_are_reachable_from_a_downstream_crate() {
    // 定数として引ける（下流が文字列リテラルを書き写さずに済む）。
    assert_eq!(codes::UNBALANCED, "unbalanced");
    assert_eq!(codes::ALREADY_REVERSED, "already_reversed");
    assert_eq!(codes::APPEND_ONLY_VIOLATION, "append_only_violation");
    assert_eq!(codes::EMPTY_REVERSE_REASON, "empty_reverse_reason");
    assert_eq!(codes::INTERNAL, "internal");

    // 4つの入口がすべて公開されている。
    let core = CoreError::Unbalanced {
        debit: "110,000".to_string(),
        credit: "100,000".to_string(),
        diff: "10,000".to_string(),
    };
    assert_eq!(core_error_code(&core), codes::UNBALANCED);
    assert_eq!(
        policy_error_code(&PolicyError::Unsupported {
            reason: "未実装".to_string()
        }),
        codes::POLICY_UNSUPPORTED
    );
    assert_eq!(
        RepoError::AppendOnlyViolation {
            reason: "UPDATE は拒否されました".to_string()
        }
        .code(),
        codes::APPEND_ONLY_VIOLATION
    );
    assert_eq!(AppError::Core(core).code(), codes::UNBALANCED);
}

// ---- 2. `#[non_exhaustive]` の受け皿が効くこと ----

#[test]
fn the_non_exhaustive_fallback_actually_catches_unmapped_variants_downstream() {
    // 下流が写像していないバリアントは `internal` に落ちる（コンパイルは通る）。
    let unmapped = AppError::Rejected {
        reason: "業務ルール違反".to_string(),
    };
    assert_eq!(downstream_error_code(&unmapped), codes::INTERNAL);

    let unmapped = AppError::Inconsistent {
        debit: "110,000".to_string(),
        credit: "100,000".to_string(),
    };
    assert_eq!(downstream_error_code(&unmapped), codes::INTERNAL);

    // 一方、`kaikei-app` が持つ完全な写像は固有のコードを返す。
    // → 下流は自前で `match` を書かず `err.code()` を呼べばよい（D-072）。
    assert_eq!(
        AppError::Rejected {
            reason: "業務ルール違反".to_string()
        }
        .code(),
        codes::REJECTED
    );
    assert_eq!(
        AppError::Inconsistent {
            debit: "110,000".to_string(),
            credit: "100,000".to_string(),
        }
        .code(),
        codes::INCONSISTENT
    );

    // 写像済みのバリアントは下流の `match` でも同じコードになる。
    assert_eq!(
        downstream_error_code(&AppError::EmptyReverseReason),
        AppError::EmptyReverseReason.code()
    );
}

// ---- 3・4. ユースケースの呼び出しと戻り値 ----

#[tokio::test]
async fn post_entry_output_is_consumable_from_a_downstream_crate() {
    let store = InMemoryStore::with_chart(chart());

    let input = PostEntryInput {
        entry_date: AccountingDate::new(2026, 4, 1).unwrap(),
        description: "現金売上".to_string(),
        lines: balanced_lines(),
        auto_tax_lines: false,
    };

    let output: PostEntryOutput = with_tx_err(&store, |tx| {
        let tax = NoTaxPolicy;
        let schema = TagSchema::empty();
        let id_gen = SequentialIdGenerator::starting_at(1);
        let clk = clock();
        let cfg = settings();
        Box::pin(
            async move { post_entry::execute(tx, &tax, &schema, &id_gen, &clk, &cfg, input).await },
        )
    })
    .await
    .unwrap();

    // MCP の応答に詰める値がすべて外から取れること。
    let entry_id = entry_id_to_uuid_string(output.entry.id());
    assert_eq!(entry_id.len(), 36, "UUID の正準表記であること");
    assert_eq!(output.entry.entry_no().as_u32(), 1);
    assert_eq!(output.entry.fiscal_year(), 2026);
    assert!(output.notes.is_empty());
    // `PolicyNote` のフィールドが外から読めること（`kaikei-app` の
    // 再エクスポート経由でも `kaikei-policy` 直参照でも同じ型）。
    let _: &[kaikei_app::PolicyNote] = &output.notes;
}

#[tokio::test]
async fn reverse_entry_output_and_empty_reason_rejection_are_visible_downstream() {
    let store = InMemoryStore::with_chart(chart());
    let original_id = seed_entry(&store, 1, AccountingDate::new(2026, 4, 1).unwrap()).await;

    // 空白のみの理由は app 層が拒否する（MCP 層で検証を重ねる必要が無い）。
    let blank = ReverseEntryInput {
        original_id,
        reverse_date: AccountingDate::new(2026, 4, 5).unwrap(),
        reason: "\u{3000}".to_string(),
        allow_double_reversal: false,
    };
    let rejected: Result<ReverseEntryOutput, AppError> = with_tx(&store, |tx| {
        // `with_tx` のクロージャは所有値しかキャプチャできないため、
        // 依存はここで組み立てる（`tx.rs` の doc「クロージャに渡せるもの」）。
        let schema = TagSchema::empty();
        let id_gen = SequentialIdGenerator::starting_at(100);
        let clk = clock();
        let cfg = settings();
        Box::pin(
            async move { reverse_entry::execute(tx, &schema, &id_gen, &clk, &cfg, blank).await },
        )
    })
    .await;
    let err = rejected.unwrap_err();
    assert_eq!(err.code(), codes::EMPTY_REVERSE_REASON);
    // メッセージは「次の手が分かる」文言（`CLAUDE.md` §11）。
    assert!(err.to_string().contains("reason"));

    // 理由があれば成功し、戻り値から赤伝が取れる。
    let ok = ReverseEntryInput {
        original_id,
        reverse_date: AccountingDate::new(2026, 4, 5).unwrap(),
        reason: "請求金額の誤り（税率の適用誤り）".to_string(),
        allow_double_reversal: false,
    };
    let output: ReverseEntryOutput = with_tx(&store, |tx| {
        let schema = TagSchema::empty();
        let id_gen = SequentialIdGenerator::starting_at(100);
        let clk = clock();
        let cfg = settings();
        Box::pin(async move { reverse_entry::execute(tx, &schema, &id_gen, &clk, &cfg, ok).await })
    })
    .await
    .unwrap();

    assert_eq!(output.entry.reverses(), Some(original_id));
    assert_eq!(
        entry_id_to_uuid_string(output.entry.reverses().unwrap()),
        entry_id_to_uuid_string(original_id)
    );
}

#[tokio::test]
async fn not_found_message_carries_a_canonical_uuid_downstream() {
    let store = InMemoryStore::with_chart(chart());
    let missing =
        entry_id_from_uuid(uuid::Uuid::parse_str("0192a7b3-1234-7abc-8def-0123456789ab").unwrap());

    let input = ReverseEntryInput {
        original_id: missing,
        reverse_date: AccountingDate::new(2026, 4, 5).unwrap(),
        reason: "存在しない仕訳".to_string(),
        allow_double_reversal: false,
    };

    let result: Result<ReverseEntryOutput, AppError> = with_tx(&store, |tx| {
        let schema = TagSchema::empty();
        let id_gen = SequentialIdGenerator::starting_at(1);
        let clk = clock();
        let cfg = settings();
        Box::pin(
            async move { reverse_entry::execute(tx, &schema, &id_gen, &clk, &cfg, input).await },
        )
    })
    .await;

    let err = result.unwrap_err();
    assert_eq!(err.code(), codes::NOT_FOUND);
    assert!(
        err.to_string()
            .contains("0192a7b3-1234-7abc-8def-0123456789ab"),
        "AI が送った UUID と突き合わせられる表記であること: {err}"
    );
}

// ---- 5. 応答の JSON を kaikei-app だけで組み立てられる（PR-B 2巡目） ----

/// `kaikei-mcp` の `tools/post_journal_entry.rs` が書くであろう
/// 「確定後の明細 → 応答 DTO」の詰め替えを、**`kaikei-app` の公開 API だけ**で
/// 実際に書いてみる。
///
/// ここで独自の `match`（`Side::Debit => "debit"` 等）や `uuid` への直接依存が
/// 要るなら、それは契約が足りていないということ（同じ表が MCP / API /
/// audit_log の3箇所に手書きされ、いずれ綴りがずれる）。
fn render_line(line: &JournalLine) -> String {
    format!(
        r#"{{"account":"{}","side":"{}","amount":"{}"}}"#,
        line.account().as_str(),
        side_code(line.side()),
        money_to_plain_string(line.amount()),
    )
}

#[test]
fn a_response_dto_can_be_built_from_kaikei_app_alone() {
    let line = JournalLine::new(
        AccountCode::parse("100").unwrap(),
        Side::Debit,
        Money::from_minor(110_000, Currency::JPY),
        TagSet::new(),
        None,
    )
    .unwrap();

    // docs/07-mcp-server.md §5: 機械可読フィールドは区切り無しの文字列。
    assert_eq!(
        render_line(&line),
        r#"{"account":"100","side":"debit","amount":"110000"}"#
    );

    // 列挙型の機械可読名（docs/07 §3 が値まで定めているもの）。
    assert_eq!(side_code(Side::Credit), "credit");
    assert_eq!(note_severity_code(NoteSeverity::Warning), "warning");
    assert_eq!(note_severity_code(NoteSeverity::Info), "info");
    assert_eq!(account_type_code(AccountType::Revenue), "revenue");
    assert_eq!(
        fiscal_year_rule_code(FiscalYearRule::CalendarYear),
        "calendar_year"
    );

    // 入力側（文字列 → 値）も引ける。設定ファイルやツール入力の解釈に使う。
    assert_eq!(side_from_code("credit").unwrap(), Side::Credit);
    assert_eq!(
        account_type_from_code("expense").unwrap(),
        AccountType::Expense
    );
    assert_eq!(
        fiscal_year_rule_from_code("calendar_year").unwrap(),
        FiscalYearRule::CalendarYear
    );
    // 未知の値は既定値に落ちず、有効な値を列挙したエラーになる。
    assert!(side_from_code("dr")
        .unwrap_err()
        .to_string()
        .contains("debit"));
}

/// エラー応答の金額欄も、成功応答と**同じ形式**に揃えられること。
///
/// `CoreError::Unbalanced` が持つのは `Money` ではなく整形済み文字列
/// （`"110,000"`）なので、区切りを外す入口が要る（`kaikei_app::amount`）。
#[test]
fn the_unbalanced_error_amounts_can_be_rendered_in_the_same_format_as_success() {
    let err = AppError::Core(CoreError::Unbalanced {
        debit: "110,000".to_string(),
        credit: "100,000".to_string(),
        diff: "10,000".to_string(),
    });
    assert_eq!(err.code(), codes::UNBALANCED);

    match &err {
        AppError::Core(CoreError::Unbalanced {
            debit,
            credit,
            diff,
        }) => {
            assert_eq!(strip_thousands_separators(debit), "110000");
            assert_eq!(strip_thousands_separators(credit), "100000");
            assert_eq!(strip_thousands_separators(diff), "10000");
        }
        other => panic!("Unbalanced を期待したが: {other:?}"),
    }

    // 区切り付きの表記は `message` の文中でだけ使う（docs/07 §5）。
    assert!(err.public_message().contains("110,000"));
}

/// 仕訳IDは**入力側も**この crate から扱える。
///
/// 1巡目は出力（`entry_id_to_uuid_string`）しか無く、下流が
/// `uuid::Uuid::parse_str` を直に書くしかなかった。`kaikei-app` は `uuid` を
/// 再エクスポートしていないので、それは下流に `uuid` 依存を強いる
/// （`DECISIONS.md` D-047 が潰したのと同じ状態）。
#[test]
fn entry_ids_can_be_parsed_and_rendered_through_kaikei_app() {
    let text = "0192a7b3-1234-7abc-8def-0123456789ab";
    let id = entry_id_from_uuid_string(text).expect("正準表記を受理すること");
    assert_eq!(entry_id_to_uuid_string(id), text);

    // 「UUID ですらない」は NotFound とは別のコードになる
    // （AI が取るべき次の手が違う）。
    let err = entry_id_from_uuid_string("42").unwrap_err();
    assert_eq!(err.code(), codes::INVALID_ENTRY_ID);
    assert_ne!(err.code(), codes::NOT_FOUND);
}

// ---- 6. エラー本文の外向きの入口 ----

/// `docs/07-mcp-server.md` §3（`message` は `Display` を写像したもの）と
/// §9（接続文字列を含みうる下位層のエラー本文をそのまま転記しない）は、
/// 入口が1つしか無いと両立しない。`public_message` がその答えである。
#[test]
fn public_message_is_safe_to_put_on_the_wire() {
    const SECRET: &str = "postgres://kaikei_app:s3cret@db.internal:5432/kaikei";

    // 下位層の生メッセージを含みうるもの: 正規化される。
    let backend = AppError::Repo(RepoError::Backend {
        reason: format!("未分類のデータベースエラーです（SQLSTATE 08006）: {SECRET}"),
    });
    assert_eq!(backend.code(), codes::BACKEND);
    assert!(backend.to_string().contains(SECRET), "診断用には残ること");
    assert!(
        !backend.public_message().contains(SECRET),
        "応答に生メッセージが漏れている: {}",
        backend.public_message()
    );

    // ドメインのエラー: そのまま出してよい（言い換えない。CLAUDE.md §10）。
    let unbalanced = AppError::Core(CoreError::Unbalanced {
        debit: "110,000".to_string(),
        credit: "100,000".to_string(),
        diff: "10,000".to_string(),
    });
    assert_eq!(unbalanced.public_message(), unbalanced.to_string());
}

// ---- 7・8. dry-run と失敗経路の PolicyNote ----

/// 税込経理の設定（`derive_tax_lines` が入力明細をそのまま返す）を最小再現した
/// `TaxPolicy`。`docs/07-mcp-server.md` §3 の
/// 「税込経理または免税事業者の設定では同じリクエストが貸借不一致になる」場面。
struct InclusiveModeTaxPolicy;

impl TaxPolicy for InclusiveModeTaxPolicy {
    fn validate_tag(
        &self,
        _ctx: &TaxContext<'_>,
        _tags: &TagSet,
        _account: &AccountDef,
    ) -> Result<(), PolicyError> {
        Ok(())
    }

    fn derive_tax_lines(
        &self,
        _ctx: &TaxContext<'_>,
        lines: &[JournalLine],
    ) -> Result<TaxDerivation, PolicyError> {
        Ok(TaxDerivation {
            lines: lines.to_vec(),
            notes: vec![PolicyNote {
                severity: NoteSeverity::Info,
                message: "税込経理の設定のため税額行を生成していません".to_string(),
            }],
        })
    }

    fn round_mode(&self, _ctx: &TaxContext<'_>) -> RoundMode {
        RoundMode::Floor
    }
}

fn unbalanced_lines() -> Vec<JournalLine> {
    vec![
        JournalLine::new(
            AccountCode::parse("100").unwrap(),
            Side::Debit,
            Money::from_minor(110_000, Currency::JPY),
            TagSet::new(),
            None,
        )
        .unwrap(),
        JournalLine::new(
            AccountCode::parse("500").unwrap(),
            Side::Credit,
            Money::from_minor(100_000, Currency::JPY),
            TagSet::new(),
            None,
        )
        .unwrap(),
    ]
}

/// ★1巡目の最大の指摘への回答★
///
/// `hint`（修正案）を組み立てるのに、MCP 層が `load_posting_context` を呼んだり
/// `TaxContext` を自作したり `sum_money` で検算したりする必要が無いこと
/// （`docs/07-mcp-server.md` §4「MCP はビジネスロジックを書かない」）。
///
/// このテストが `kaikei-app` の**ユースケース関数2本を呼ぶだけ**で書けている
/// ことが、契約が足りていることの証明になっている。
#[tokio::test]
async fn a_downstream_crate_can_build_a_hint_without_writing_business_logic() {
    let store = InMemoryStore::with_chart(chart());
    let entry_date = AccountingDate::new(2026, 4, 1).unwrap();

    // (1) auto_tax_lines: false の post が貸借不一致で失敗する。
    let failure = {
        let input = PostEntryInput {
            entry_date,
            description: "税抜金額のみ".to_string(),
            lines: unbalanced_lines(),
            auto_tax_lines: false,
        };
        let result: Result<PostEntryOutput, PostEntryFailure> = with_tx_err(&store, |tx| {
            let tax = NoTaxPolicy;
            let schema = TagSchema::empty();
            let id_gen = SequentialIdGenerator::starting_at(1);
            let clk = clock();
            let cfg = settings();
            Box::pin(async move {
                post_entry::execute(tx, &tax, &schema, &id_gen, &clk, &cfg, input).await
            })
        })
        .await;
        result.unwrap_err()
    };
    assert_eq!(failure.code(), codes::UNBALANCED);

    // (2) 同じ明細を auto_tax_lines: true で dry-run する。
    let previewed: Result<PreviewEntryOutput, PostEntryFailure> = with_tx_err(&store, |tx| {
        let tax = InclusiveModeTaxPolicy;
        let schema = TagSchema::empty();
        let clk = clock();
        let cfg = settings();
        let input = PostEntryInput {
            entry_date,
            description: "税抜金額のみ".to_string(),
            lines: unbalanced_lines(),
            auto_tax_lines: true,
        };
        Box::pin(async move { post_entry::preview(tx, &tax, &schema, &clk, &cfg, input).await })
    })
    .await;

    // (3) この policy 設定では税額行が生成されないので hint は出せない。
    //     ただし**なぜ出せないのか**は `notes` から分かる（★失敗経路の PolicyNote★）。
    let preview_failure = previewed.unwrap_err();
    assert_eq!(preview_failure.code(), codes::UNBALANCED);
    assert_eq!(
        preview_failure.notes.len(),
        1,
        "失敗経路で PolicyNote が落ちている"
    );
    assert_eq!(
        note_severity_code(preview_failure.notes[0].severity),
        "info"
    );
    assert!(preview_failure.notes[0].message.contains("税込経理"));

    // dry-run は帳簿に触れていない。
    assert!(store.committed_entries().is_empty());
}

/// 税抜経理の設定なら、dry-run が `hint.suggested_lines` に載せる明細を返す。
#[tokio::test]
async fn preview_returns_the_lines_a_hint_would_carry() {
    let store = InMemoryStore::with_chart(chart_with_tax_account());
    let entry_date = AccountingDate::new(2026, 4, 1).unwrap();

    let previewed: Result<PreviewEntryOutput, PostEntryFailure> = with_tx_err(&store, |tx| {
        let tax = FlatRateTaxPolicy {
            rate: Ratio::parse_rate("0.10").unwrap(),
            tax_account: "330",
        };
        let schema = TagSchema::empty();
        let clk = clock();
        let cfg = settings();
        let input = PostEntryInput {
            entry_date,
            description: "税抜経理".to_string(),
            lines: balanced_lines(),
            auto_tax_lines: true,
        };
        Box::pin(async move { post_entry::preview(tx, &tax, &schema, &clk, &cfg, input).await })
    })
    .await;

    let previewed = previewed.unwrap();
    assert_eq!(previewed.lines.len(), 4, "税額行が追加される");
    assert_eq!(previewed.debit_total, previewed.credit_total);
    // 応答に載せる形（区切り無しの文字列）まで kaikei-app だけで作れる。
    assert_eq!(money_to_plain_string(&previewed.debit_total), "1100");
    // dry-run なので記帳されていない。
    assert!(store.committed_entries().is_empty());
}

// ---- 試算表（C-7） ----

/// 0行の期間でも応答で通貨を名乗れること（`get_trial_balance` の
/// `currency` フィールド）。1巡目は `totals()` が `Ok(None)` を返すため
/// 名乗れなかった。
#[tokio::test]
async fn an_empty_trial_balance_still_reports_its_currency_downstream() {
    struct EmptyQuery;

    #[async_trait]
    impl TrialBalanceQuery for EmptyQuery {
        async fn trial_balance(
            &self,
            _from: AccountingDate,
            _to: AccountingDate,
            _group_by: &[TagKey],
        ) -> Result<Vec<BalanceRowView>, RepoError> {
            Ok(Vec::new())
        }
    }

    let view = report::execute(
        &EmptyQuery,
        &TagSchema::empty(),
        &settings(),
        ReportInput {
            from: AccountingDate::new(2026, 1, 1).unwrap(),
            to: AccountingDate::new(2026, 12, 31).unwrap(),
            group_by: Vec::new(),
        },
    )
    .await
    .unwrap();

    assert!(view.rows().is_empty());
    assert_eq!(view.currency().code(), "JPY");
    let (debit, credit) = view.totals().unwrap();
    assert_eq!(money_to_plain_string(&debit), "0");
    assert_eq!(money_to_plain_string(&credit), "0");
}
