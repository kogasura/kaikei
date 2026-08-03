//! ★契約凍結点★ の検証: PR-B で確定した契約が、**下流の crate から見て**
//! そのまま使えることを確かめる。
//!
//! 統合テスト（`tests/*.rs`）は `kaikei-app` を外部 crate としてリンクするため、
//! ここでのコンパイル可否は `kaikei-mcp` / `kaikei-api` から見た可否と同じになる
//! （`#[cfg(test)]` は効かず、`pub` でないものは見えない）。
//! `PROGRESS.md` Phase 1 の教訓2「契約を凍結する前に、その契約を使う側の
//! コードを実際に書いてみる」に従う。
//!
//! ここで踏むのは次の4点:
//!
//! 1. エラーコードの語彙（`codes` の定数と4つの入口）が外から引けること
//! 2. **`#[non_exhaustive]` の受け皿が実際に効くこと。** 下流の `match` では
//!    `_` の腕が必須であり、しかも「到達しない」と警告されない
//!    （定義元 crate 内では逆に警告されるため、この検証はここでしかできない）
//! 3. `BookSettings` を外から構築できること（帳簿通貨を含む）
//! 4. 書き込み系ユースケースの戻り値（`PostEntryOutput` / `ReverseEntryOutput`）
//!    のフィールドが外から読めること

use kaikei_app::context::{BookSettings, FiscalYearRule};
use kaikei_app::currency::currency_from_code;
use kaikei_app::error::{codes, core_error_code, policy_error_code, AppError, RepoError};
use kaikei_app::id::{entry_id_from_uuid, entry_id_to_uuid_string};
use kaikei_app::ports::{JournalRepo, NumberingRepo, Store, TxScope};
use kaikei_app::testing::{InMemoryStore, SequentialIdGenerator};
use kaikei_app::tx::with_tx;
use kaikei_app::usecase::post_entry::{self, PostEntryInput, PostEntryOutput};
use kaikei_app::usecase::reverse_entry::{self, ReverseEntryInput, ReverseEntryOutput};
use kaikei_core::{
    AccountCode, AccountDef, AccountType, AccountingDate, ChartOfAccounts, CoreError, Currency,
    EntryId, FixedClock, JournalEntry, JournalLine, Money, NewEntry, PeriodGuard, PeriodStatus,
    Side, TagSchema, TagSet, Timestamp,
};
use kaikei_policy::testing::NoTaxPolicy;
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

    let output: PostEntryOutput = with_tx(&store, |tx| {
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
