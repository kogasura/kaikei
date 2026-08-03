//! `kaikei_store::query::{PgSearchEntriesQuery, PgLedgerQuery}`（SQL）と、
//! **同じ仕訳集合をドメインモデル（`kaikei_core::JournalEntry`）の上で
//! 走査した結果**が一致することを検証する差分テスト
//! （`tests/trial_balance_differential.rs` と同じ水準・同じ手法）。
//!
//! # 対照実装は「core に既にあるもの」ではない
//!
//! 試算表には `kaikei_core::TrialBalance::from_entries` という core 側の
//! 実装があり、あちらはそれと突き合わせられた。検索・元帳には対応する
//! core の実装が無い（`CLAUDE.md` §1 により、この PR で core に足すことも
//! しない）。そこで対照実装を**このテストの中に素朴に書く**:
//!
//! | 見るもの | 対照実装 |
//! |---|---|
//! | 検索の絞り込み | 構築済み `JournalEntry` の `Vec` を `filter` で絞る |
//! | 元帳の並びと残高 | 同じ `Vec` から対象科目の明細を取り出し、順に加減する |
//!
//! **素朴な実装であることに意味がある。** SQL 側は `EXISTS` / `unnest` /
//! ウィンドウ関数 / keyset ページングで書かれており、対照側は
//! 「仕訳を順に見て条件を確かめる」だけである。両者が一致することは、
//! SQL の最適化された書き方が素朴な意味と同じであることの証拠になる。
//!
//! # ページングも差分の対象にする
//!
//! 1ページずつ最後まで辿った結果が、上限を十分に大きくして一度に取った
//! 結果と**完全に一致する**ことを見る（`DECISIONS.md` D-089）。
//! 取りこぼし・重複はここで落ちる。

#![cfg(feature = "pg-tests")]

mod common;

use common::AllOpen;
use kaikei_app::error::RepoError;
use kaikei_app::id::entry_id_to_uuid_string;
use kaikei_app::ports::{LedgerParams, LedgerQuery, SearchEntriesParams, SearchEntriesQuery};
use kaikei_app::view::{EntryCursor, LedgerCursor};
use kaikei_core::{
    AccountCode, AccountDef, AccountType, AccountingDate, ChartOfAccounts, Currency, EntryId,
    EntryNumber, FiscalYear, FixedClock, JournalEntry, JournalLine, Money, NewEntry, Side, TagDef,
    TagKey, TagSchema, TagSet, TagValue, TagValueType, Timestamp,
};
use kaikei_store::convert::{
    account_type_to_i16, accounting_date_to_naive_date, entry_no_to_i32, money_to_columns,
    side_to_i16, timestamp_to_datetime,
};
use kaikei_store::query::{PgLedgerQuery, PgSearchEntriesQuery};
use kaikei_store::tags::tag_set_to_json;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::PgPool;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// 素材（`tests/trial_balance_differential.rs` と同じ組み立て）
// ---------------------------------------------------------------------------

fn code(s: &str) -> AccountCode {
    AccountCode::parse(s).unwrap()
}

fn key(s: &str) -> TagKey {
    TagKey::parse(s).unwrap()
}

fn date(y: i32, m: u8, d: u8) -> AccountingDate {
    AccountingDate::new(y, m, d).unwrap()
}

fn account(code_str: &str, name: &str, account_type: AccountType) -> AccountDef {
    AccountDef {
        code: code(code_str),
        name: name.to_string(),
        account_type,
        parent: None,
        postable: true,
    }
}

fn line(account_code: &str, side: Side, amount: i128, tags: TagSet) -> JournalLine {
    JournalLine::new(
        code(account_code),
        side,
        Money::from_minor(amount, Currency::JPY),
        tags,
        None,
    )
    .unwrap()
}

fn counterparty(value: &str) -> TagSet {
    let mut tags = TagSet::new();
    tags.insert(key("counterparty"), TagValue::Code(value.to_string()));
    tags
}

fn schema() -> TagSchema {
    TagSchema::new(vec![(
        key("counterparty"),
        TagDef {
            value_type: TagValueType::Code,
            aggregatable: true,
            required_for: vec![],
        },
    )])
}

fn chart() -> ChartOfAccounts {
    ChartOfAccounts::new(vec![
        account("100", "現金", AccountType::Asset),
        account("500", "売上高", AccountType::Revenue),
        account("600", "消耗品費", AccountType::Expense),
    ])
    .unwrap()
}

#[allow(clippy::too_many_arguments)]
fn entry(
    id: u128,
    entry_no: u32,
    entry_date: AccountingDate,
    description: &str,
    lines: Vec<JournalLine>,
    fy: &FiscalYear,
    chart: &ChartOfAccounts,
    schema: &TagSchema,
) -> JournalEntry {
    JournalEntry::new(
        NewEntry {
            id: EntryId::new(id),
            entry_no: EntryNumber::new(entry_no),
            entry_date,
            description: description.to_string(),
            lines,
            document_refs: Vec::new(),
        },
        fy,
        chart,
        schema,
        &AllOpen,
        &FixedClock(Timestamp::from_unix_nanos(0)),
    )
    .unwrap()
}

async fn insert_account(pool: &PgPool, def: &AccountDef) {
    sqlx::query(
        "INSERT INTO accounts (code, name, account_type, parent_code, postable) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(def.code.as_str())
    .bind(&def.name)
    .bind(account_type_to_i16(def.account_type))
    .bind(def.parent.as_ref().map(AccountCode::as_str))
    .bind(def.postable)
    .execute(pool)
    .await
    .unwrap();
}

/// 構築済みの仕訳を、`kaikei-store` の共有変換関数を使って DB へ INSERT する
/// （`tests/trial_balance_differential.rs` と同じ形）。
async fn insert_entry(pool: &PgPool, entry: &JournalEntry) {
    let mut tx = pool.begin().await.unwrap();

    let id = Uuid::from_u128(entry.id().as_u128());
    sqlx::query(
        "INSERT INTO journal_entries \
         (id, fiscal_year, entry_no, entry_date, description, reverses, reverse_reason, recorded_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(id)
    .bind(entry.fiscal_year())
    .bind(entry_no_to_i32(entry.entry_no()).unwrap())
    .bind(accounting_date_to_naive_date(entry.entry_date()).unwrap())
    .bind(entry.description())
    .bind(entry.reverses().map(|r| Uuid::from_u128(r.as_u128())))
    .bind(entry.reverse_reason())
    .bind(timestamp_to_datetime(entry.recorded_at()).unwrap())
    .execute(&mut *tx)
    .await
    .unwrap();

    for (i, line) in entry.lines().iter().enumerate() {
        let line_no = i16::try_from(i + 1).unwrap();
        let (amount_minor, currency, minor_unit) = money_to_columns(line.amount()).unwrap();
        sqlx::query(
            "INSERT INTO journal_lines \
             (entry_id, line_no, account_code, side, amount_minor, currency, \
              currency_minor_unit, tags, memo) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(id)
        .bind(line_no)
        .bind(line.account().as_str())
        .bind(side_to_i16(line.side()))
        .bind(amount_minor)
        .bind(currency)
        .bind(minor_unit)
        .bind(tag_set_to_json(line.tags()))
        .bind(line.memo())
        .execute(&mut *tx)
        .await
        .unwrap();
    }

    tx.commit().await.unwrap();
}

/// 差分テストの土台。5件の仕訳（うち1件は赤伝で取り消し済み）を作る。
async fn seed(pool: &PgPool) -> Vec<JournalEntry> {
    let chart = chart();
    for def in chart.iter() {
        insert_account(pool, def).await;
    }
    let schema = schema();
    let fy = FiscalYear::calendar_year(2026);

    let mut entries = vec![
        entry(
            1,
            1,
            date(2026, 1, 10),
            "A社への請求",
            vec![
                line("100", Side::Debit, 10_000, TagSet::new()),
                line("500", Side::Credit, 10_000, counterparty("CP0001")),
            ],
            &fy,
            &chart,
            &schema,
        ),
        entry(
            2,
            2,
            date(2026, 2, 1),
            "文具の購入",
            vec![
                line("600", Side::Debit, 1_500, counterparty("CP0002")),
                line("100", Side::Credit, 1_500, TagSet::new()),
            ],
            &fy,
            &chart,
            &schema,
        ),
        entry(
            3,
            3,
            date(2026, 2, 1),
            "B社への請求",
            vec![
                line("100", Side::Debit, 3_000, TagSet::new()),
                line("500", Side::Credit, 3_000, counterparty("CP0002")),
            ],
            &fy,
            &chart,
            &schema,
        ),
        entry(
            4,
            4,
            date(2026, 3, 20),
            "消耗品の購入（100%割引の交渉メモ_A）",
            vec![
                line("600", Side::Debit, 800, TagSet::new()),
                line("100", Side::Credit, 800, TagSet::new()),
            ],
            &fy,
            &chart,
            &schema,
        ),
    ];

    // 4件目を赤伝で取り消す（帳簿は追記のみ。元仕訳も残る）。
    let reversal = entries[3]
        .reverse(
            EntryId::new(5),
            EntryNumber::new(5),
            date(2026, 3, 31),
            "数量の誤り".to_string(),
            &fy,
            &chart,
            &schema,
            &AllOpen,
            &FixedClock(Timestamp::from_unix_nanos(0)),
        )
        .unwrap();
    entries.push(reversal);

    for e in &entries {
        insert_entry(pool, e).await;
    }
    entries
}

fn params(limit: u32) -> SearchEntriesParams {
    SearchEntriesParams {
        from: None,
        to: None,
        account: None,
        description_contains: None,
        min_amount: None,
        max_amount: None,
        tags: Vec::new(),
        cursor: None,
        limit,
    }
}

/// 対照実装の並び順（SQL の `ORDER BY entry_date, entry_no, id` と同じ）。
fn sorted_ids(entries: &[&JournalEntry]) -> Vec<String> {
    let mut sorted: Vec<&&JournalEntry> = entries.iter().collect();
    sorted.sort_by_key(|e| (e.entry_date(), e.entry_no(), e.id().as_u128()));
    sorted
        .into_iter()
        .map(|e| entry_id_to_uuid_string(e.id()))
        .collect()
}

// ---------------------------------------------------------------------------
// 検索の差分
// ---------------------------------------------------------------------------

/// 主戦場: 条件なしの検索が、構築した仕訳集合そのもの（並び順も含めて）と
/// 一致する。明細も1件ずつ突き合わせる。
#[sqlx::test]
async fn search_without_conditions_matches_every_entry_in_order(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;
    let entries = seed(&roles.migrator).await;

    let query = PgSearchEntriesQuery::new(roles.app.clone());
    let page = query.search_entries(&params(100)).await.unwrap();

    assert_eq!(page.total_matches, entries.len() as u64);
    assert!(page.next_cursor.is_none(), "全件が1ページに収まる");
    assert_eq!(
        page.entries
            .iter()
            .map(|e| entry_id_to_uuid_string(e.entry_id))
            .collect::<Vec<_>>(),
        sorted_ids(&entries.iter().collect::<Vec<_>>()),
    );

    // 明細（科目・貸借・金額・タグ）が1行ずつ一致する。
    for view in &page.entries {
        let expected = entries
            .iter()
            .find(|e| e.id() == view.entry_id)
            .expect("同じ仕訳がある");
        assert_eq!(view.entry_no, expected.entry_no());
        assert_eq!(view.fiscal_year, expected.fiscal_year());
        assert_eq!(view.entry_date, expected.entry_date());
        assert_eq!(view.description, expected.description());
        assert_eq!(view.lines.len(), expected.lines().len());
        for (got, want) in view.lines.iter().zip(expected.lines()) {
            assert_eq!(got.account(), want.account());
            assert_eq!(got.side(), want.side());
            assert_eq!(got.amount(), want.amount());
            assert_eq!(
                got.tags().iter().collect::<Vec<_>>(),
                want.tags().iter().collect::<Vec<_>>()
            );
        }
    }
}

/// 各絞り込みが、同じ条件で `Vec<JournalEntry>` を `filter` した結果と一致する。
#[sqlx::test]
async fn every_filter_matches_the_same_predicate_applied_to_the_domain_model(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;
    let entries = seed(&roles.migrator).await;

    let run = |params: SearchEntriesParams| {
        let query = PgSearchEntriesQuery::new(roles.app.clone());
        async move { query.search_entries(&params).await.unwrap() }
    };

    // (1) 期間（取引日）。
    let mut by_period = params(100);
    by_period.from = Some(date(2026, 2, 1));
    by_period.to = Some(date(2026, 3, 20));
    let expected: Vec<&JournalEntry> = entries
        .iter()
        .filter(|e| e.entry_date() >= date(2026, 2, 1) && e.entry_date() <= date(2026, 3, 20))
        .collect();
    let page = run(by_period).await;
    assert_eq!(
        page.entries
            .iter()
            .map(|e| entry_id_to_uuid_string(e.entry_id))
            .collect::<Vec<_>>(),
        sorted_ids(&expected)
    );
    assert_eq!(page.total_matches, expected.len() as u64);

    // (2) 勘定科目。
    let mut by_account = params(100);
    by_account.account = Some(code("600"));
    let expected: Vec<&JournalEntry> = entries
        .iter()
        .filter(|e| e.lines().iter().any(|l| l.account() == &code("600")))
        .collect();
    let page = run(by_account).await;
    assert_eq!(
        page.entries
            .iter()
            .map(|e| entry_id_to_uuid_string(e.entry_id))
            .collect::<Vec<_>>(),
        sorted_ids(&expected)
    );

    // (3) 金額の範囲（明細1行の金額と比較する）。
    let mut by_amount = params(100);
    by_amount.min_amount = Some(Money::from_minor(1_000, Currency::JPY));
    by_amount.max_amount = Some(Money::from_minor(3_000, Currency::JPY));
    let expected: Vec<&JournalEntry> = entries
        .iter()
        .filter(|e| {
            e.lines()
                .iter()
                .any(|l| (1_000..=3_000).contains(&l.amount().minor()))
        })
        .collect();
    let page = run(by_amount).await;
    assert_eq!(
        page.entries
            .iter()
            .map(|e| entry_id_to_uuid_string(e.entry_id))
            .collect::<Vec<_>>(),
        sorted_ids(&expected)
    );

    // (4) 摘要の部分一致。
    let mut by_description = params(100);
    by_description.description_contains = Some("請求".to_string());
    let expected: Vec<&JournalEntry> = entries
        .iter()
        .filter(|e| e.description().contains("請求"))
        .collect();
    let page = run(by_description).await;
    assert_eq!(
        page.entries
            .iter()
            .map(|e| entry_id_to_uuid_string(e.entry_id))
            .collect::<Vec<_>>(),
        sorted_ids(&expected)
    );

    // (5) タグ。
    let mut by_tag = params(100);
    by_tag.tags = vec![(key("counterparty"), "CP0002".to_string())];
    let expected: Vec<&JournalEntry> = entries
        .iter()
        .filter(|e| {
            e.lines().iter().any(|l| {
                l.tags().get(&key("counterparty")) == Some(&TagValue::Code("CP0002".to_string()))
            })
        })
        .collect();
    let page = run(by_tag).await;
    assert_eq!(
        page.entries
            .iter()
            .map(|e| entry_id_to_uuid_string(e.entry_id))
            .collect::<Vec<_>>(),
        sorted_ids(&expected)
    );
    assert!(!expected.is_empty(), "対照が空では検査にならない");
}

/// 検索語に含まれる `%` / `_` はワイルドカードとして効かない
/// （効くと「多すぎる結果」が正しい結果として返る）。
#[sqlx::test]
async fn wildcards_in_the_search_term_are_matched_literally(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;
    let entries = seed(&roles.migrator).await;
    let query = PgSearchEntriesQuery::new(roles.app.clone());

    // `%` / `_` を含む検索語でも、対照（素朴な `contains`）と一致する。
    // 効いてしまうと「100」で始まる摘要すべてに広がり、**多すぎる結果が
    // 正しい結果として**返る。
    //
    // 「100%割引…メモ_A」を含む摘要は元仕訳とその赤伝（【訂正】付き）の2件。
    for term in ["100%割引", "メモ_A", "%", "_"] {
        let mut params = params(100);
        params.description_contains = Some(term.to_string());
        let page = query.search_entries(&params).await.unwrap();
        let expected = entries
            .iter()
            .filter(|e| e.description().contains(term))
            .count();
        assert_eq!(
            page.total_matches, expected as u64,
            "検索語 {term:?} でワイルドカードが効いています"
        );
    }

    // 対照実験: 該当が無い語では0件になる（`_` が1文字ワイルドカードとして
    // 効いていれば「メモXB」に当たりうるが、そのような摘要は無い）。
    assert!(
        entries.iter().all(|e| !e.description().contains("メモ_B")),
        "対照の作り方が変わっている"
    );
    let mut absent = params(100);
    absent.description_contains = Some("メモ_B".to_string());
    assert_eq!(
        query.search_entries(&absent).await.unwrap().total_matches,
        0
    );
}

/// ★ページングの差分★ 1件ずつ最後まで辿った結果が、一度に全件取った
/// 結果と完全に一致する（取りこぼしも重複も無い）。
#[sqlx::test]
async fn paging_one_entry_at_a_time_yields_exactly_the_same_sequence(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;
    let entries = seed(&roles.migrator).await;
    let query = PgSearchEntriesQuery::new(roles.app.clone());

    let all = query.search_entries(&params(100)).await.unwrap();
    assert_eq!(all.entries.len(), entries.len());

    let mut collected: Vec<String> = Vec::new();
    let mut cursor: Option<EntryCursor> = None;
    loop {
        let mut page = params(1);
        page.cursor = cursor;
        let page = query.search_entries(&page).await.unwrap();

        // 総件数はページによらず一定。
        assert_eq!(page.total_matches, all.total_matches);
        assert!(page.entries.len() <= 1);
        collected.extend(
            page.entries
                .iter()
                .map(|e| entry_id_to_uuid_string(e.entry_id)),
        );

        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
        assert!(collected.len() <= entries.len(), "ページングが終わらない");
    }

    assert_eq!(
        collected,
        all.entries
            .iter()
            .map(|e| entry_id_to_uuid_string(e.entry_id))
            .collect::<Vec<_>>()
    );
}

/// 取り消された仕訳と赤伝が、どちらも**それと分かる形**で返る
/// （`DECISIONS.md` D-088）。
#[sqlx::test]
async fn a_reversed_entry_and_its_reversal_are_both_visible_and_linked(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;
    let entries = seed(&roles.migrator).await;
    let query = PgSearchEntriesQuery::new(roles.app.clone());

    let page = query.search_entries(&params(100)).await.unwrap();

    // 帳簿は追記のみ。元仕訳も赤伝も消えない。
    assert_eq!(page.entries.len(), entries.len());

    let original = page
        .entries
        .iter()
        .find(|e| e.entry_id == EntryId::new(4))
        .expect("元仕訳が残っている");
    let reversal = original
        .reversed_by
        .as_ref()
        .expect("取り消されたことが分かる");
    assert_eq!(reversal.entry_id, EntryId::new(5));
    assert_eq!(reversal.entry_no, EntryNumber::new(5));
    assert_eq!(reversal.entry_date, date(2026, 3, 31));
    assert!(original.reverses.is_none());

    let red = page
        .entries
        .iter()
        .find(|e| e.entry_id == EntryId::new(5))
        .expect("赤伝も返る");
    assert_eq!(red.reverses, Some(EntryId::new(4)));
    assert_eq!(red.reverse_reason.as_deref(), Some("数量の誤り"));
    assert!(red.reversed_by.is_none(), "赤伝はまだ取り消されていない");

    // 取り消しに関わらない仕訳にはどちらも付かない。
    let plain = page
        .entries
        .iter()
        .find(|e| e.entry_id == EntryId::new(1))
        .unwrap();
    assert!(plain.reverses.is_none());
    assert!(plain.reversed_by.is_none());
}

/// 0件は成功（空の一覧）。「見つからない」ではない。
#[sqlx::test]
async fn a_search_with_no_hits_succeeds_with_an_empty_page(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;
    seed(&roles.migrator).await;
    let query = PgSearchEntriesQuery::new(roles.app.clone());

    let mut params = params(100);
    params.description_contains = Some("存在しない摘要".to_string());
    let page = query.search_entries(&params).await.unwrap();

    assert!(page.entries.is_empty());
    assert_eq!(page.total_matches, 0);
    assert!(page.next_cursor.is_none());
}

// ---------------------------------------------------------------------------
// 元帳の差分
// ---------------------------------------------------------------------------

fn ledger_params(account: &str, from: AccountingDate, to: AccountingDate) -> LedgerParams {
    LedgerParams {
        account: code(account),
        from,
        to,
        book_currency: Currency::JPY,
        cursor: None,
        limit: 100,
    }
}

/// 対照実装: 構築済み仕訳から、対象科目の明細を並び順どおりに取り出す。
fn expected_rows<'a>(
    entries: &'a [JournalEntry],
    account: &AccountCode,
    from: AccountingDate,
    to: AccountingDate,
) -> Vec<(&'a JournalEntry, usize, &'a JournalLine)> {
    let mut rows: Vec<(&JournalEntry, usize, &JournalLine)> = entries
        .iter()
        .filter(|e| e.entry_date() >= from && e.entry_date() <= to)
        .flat_map(|e| {
            e.lines()
                .iter()
                .enumerate()
                .filter(move |(_, l)| l.account() == account)
                .map(move |(i, l)| (e, i + 1, l))
        })
        .collect();
    rows.sort_by_key(|(e, line_no, _)| (e.entry_date(), e.entry_no(), e.id().as_u128(), *line_no));
    rows
}

/// 主戦場: 元帳の行・並び・残高の累計・期間合計が、ドメインモデルを順に
/// 加減した結果と一致する。
#[sqlx::test]
async fn ledger_rows_and_running_balance_match_the_domain_model(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;
    let entries = seed(&roles.migrator).await;
    let query = PgLedgerQuery::new(roles.app.clone());

    let from = date(2026, 1, 1);
    let to = date(2026, 12, 31);
    let page = query.ledger(&ledger_params("100", from, to)).await.unwrap();

    let expected = expected_rows(&entries, &code("100"), from, to);
    assert_eq!(page.rows.len(), expected.len());
    assert_eq!(page.total_lines, expected.len() as u64);
    assert!(page.next_cursor.is_none());
    assert_eq!(page.account_name, "現金");
    assert_eq!(page.account_type, AccountType::Asset);
    // 期首より前に明細が無いので期首残高はゼロ。
    assert_eq!(page.opening_balance.minor(), 0);

    // 行の内容と残高の累計（資産なので 借方 − 貸方）。
    let mut running = 0_i128;
    let mut debit = 0_i128;
    let mut credit = 0_i128;
    for (row, (expected_entry, line_no, expected_line)) in page.rows.iter().zip(&expected) {
        assert_eq!(row.entry_id, expected_entry.id());
        assert_eq!(row.entry_no, expected_entry.entry_no());
        assert_eq!(row.entry_date, expected_entry.entry_date());
        assert_eq!(row.line_no, *line_no as u16);
        assert_eq!(row.description, expected_entry.description());
        assert_eq!(row.side, expected_line.side());
        assert_eq!(&row.amount, expected_line.amount());

        match expected_line.side() {
            Side::Debit => {
                running += expected_line.amount().minor();
                debit += expected_line.amount().minor();
            }
            Side::Credit => {
                running -= expected_line.amount().minor();
                credit += expected_line.amount().minor();
            }
        }
        assert_eq!(row.running_balance.minor(), running, "残高の累計が食い違う");

        // 相手科目（同じ仕訳の反対側）。
        let mut want: Vec<String> = expected_entry
            .lines()
            .iter()
            .filter(|l| l.side() != expected_line.side())
            .map(|l| l.account().as_str().to_string())
            .collect();
        want.sort();
        want.dedup();
        assert_eq!(
            row.counter_accounts
                .iter()
                .map(|c| c.as_str().to_string())
                .collect::<Vec<_>>(),
            want
        );
    }

    assert_eq!(page.debit_total.minor(), debit);
    assert_eq!(page.credit_total.minor(), credit);
    assert_eq!(page.closing_balance.minor(), running);
}

/// 収益科目（貸方が正）の残高は符号が逆になる（`DOMAIN.md` §2）。
#[sqlx::test]
async fn a_credit_normal_account_reports_the_balance_with_the_opposite_sign(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;
    seed(&roles.migrator).await;
    let query = PgLedgerQuery::new(roles.app.clone());

    let page = query
        .ledger(&ledger_params("500", date(2026, 1, 1), date(2026, 12, 31)))
        .await
        .unwrap();

    assert_eq!(page.account_type, AccountType::Revenue);
    // 売上 10,000 + 3,000 はすべて貸方。収益は貸方が正。
    assert_eq!(page.credit_total.minor(), 13_000);
    assert_eq!(page.debit_total.minor(), 0);
    assert_eq!(page.closing_balance.minor(), 13_000);
    assert_eq!(page.rows.last().unwrap().running_balance.minor(), 13_000);
}

/// 期首残高は `from` より前のすべての明細から求める（期間を切っても
/// 残高が連続する）。
#[sqlx::test]
async fn the_opening_balance_carries_everything_before_the_period(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;
    let entries = seed(&roles.migrator).await;
    let query = PgLedgerQuery::new(roles.app.clone());

    let from = date(2026, 2, 1);
    let page = query
        .ledger(&ledger_params("100", from, date(2026, 12, 31)))
        .await
        .unwrap();

    // 2月1日より前は 1/10 の借方 10,000 だけ。
    let expected_opening: i128 = entries
        .iter()
        .filter(|e| e.entry_date() < from)
        .flat_map(|e| e.lines())
        .filter(|l| l.account() == &code("100"))
        .map(|l| match l.side() {
            Side::Debit => l.amount().minor(),
            Side::Credit => -l.amount().minor(),
        })
        .sum();
    assert_eq!(expected_opening, 10_000);
    assert_eq!(page.opening_balance.minor(), expected_opening);

    // 最初の行の残高は「期首残高 + その行」になる。
    let first = &page.rows[0];
    let delta = match first.side {
        Side::Debit => first.amount.minor(),
        Side::Credit => -first.amount.minor(),
    };
    assert_eq!(first.running_balance.minor(), expected_opening + delta);
}

/// ★ページングの差分★ 1行ずつ辿った結果が一度に取った結果と一致し、
/// **残高の累計もページをまたいで連続する**（ページ内で数え直さない）。
#[sqlx::test]
async fn paging_the_ledger_keeps_the_running_balance_continuous(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;
    seed(&roles.migrator).await;
    let query = PgLedgerQuery::new(roles.app.clone());

    let from = date(2026, 1, 1);
    let to = date(2026, 12, 31);
    let all = query.ledger(&ledger_params("100", from, to)).await.unwrap();
    assert!(all.rows.len() >= 4, "対照が薄すぎる");

    let mut collected = Vec::new();
    let mut cursor: Option<LedgerCursor> = None;
    loop {
        let mut params = ledger_params("100", from, to);
        params.limit = 1;
        params.cursor = cursor;
        let page = query.ledger(&params).await.unwrap();

        // 合計・行数はページによらず期間全体の値。
        assert_eq!(page.total_lines, all.total_lines);
        assert_eq!(page.debit_total, all.debit_total);
        assert_eq!(page.credit_total, all.credit_total);
        assert_eq!(page.opening_balance, all.opening_balance);
        assert_eq!(page.closing_balance, all.closing_balance);

        collected.extend(page.rows.iter().map(|r| {
            (
                r.entry_id,
                r.line_no,
                r.amount.minor(),
                r.running_balance.minor(),
            )
        }));
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
        assert!(collected.len() <= all.rows.len(), "ページングが終わらない");
    }

    let expected: Vec<_> = all
        .rows
        .iter()
        .map(|r| {
            (
                r.entry_id,
                r.line_no,
                r.amount.minor(),
                r.running_balance.minor(),
            )
        })
        .collect();
    assert_eq!(collected, expected);
}

/// 元帳にも「取り消された」ことが出る（`DECISIONS.md` D-088）。
/// 赤伝の行も残るので、合計はゼロに戻る。
#[sqlx::test]
async fn the_ledger_marks_rows_of_reversed_entries_and_keeps_both_sides(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;
    seed(&roles.migrator).await;
    let query = PgLedgerQuery::new(roles.app.clone());

    let page = query
        .ledger(&ledger_params("600", date(2026, 1, 1), date(2026, 12, 31)))
        .await
        .unwrap();

    // 消耗品費: 1,500（entry 2）+ 800（entry 4）− 800（赤伝 entry 5）。
    assert_eq!(page.rows.len(), 3);
    assert_eq!(page.closing_balance.minor(), 1_500);

    let reversed_row = page
        .rows
        .iter()
        .find(|r| r.entry_id == EntryId::new(4))
        .expect("取り消された仕訳の行も残る");
    let marker = reversed_row
        .reversed_by
        .as_ref()
        .expect("取り消されたことが分かる");
    assert_eq!(marker.entry_id, EntryId::new(5));

    let red_row = page
        .rows
        .iter()
        .find(|r| r.entry_id == EntryId::new(5))
        .expect("赤伝の行も残る");
    assert_eq!(red_row.reverses, Some(EntryId::new(4)));
    assert_eq!(red_row.side, Side::Credit, "赤伝は貸借が入れ替わる");
}

/// 明細が1行も無い期間は**成功**（0行・合計ゼロ）で、勘定科目マスタに
/// 無い科目コードは `NotFound` の**エラー**である。
#[sqlx::test]
async fn an_empty_period_succeeds_but_an_unknown_account_is_not_found(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;
    seed(&roles.migrator).await;
    let query = PgLedgerQuery::new(roles.app.clone());

    // 実在する科目・取引の無い期間 → 成功。
    let empty = query
        .ledger(&ledger_params("600", date(2027, 1, 1), date(2027, 12, 31)))
        .await
        .unwrap();
    assert!(empty.rows.is_empty());
    assert_eq!(empty.total_lines, 0);
    assert_eq!(empty.debit_total.minor(), 0);
    // 期首残高（2027年より前の累計）は残る。
    assert_eq!(empty.opening_balance.minor(), 1_500);
    assert_eq!(empty.closing_balance.minor(), 1_500);

    // 実在しない科目 → エラー（空の元帳にしない）。
    let missing = query
        .ledger(&ledger_params("999", date(2026, 1, 1), date(2026, 12, 31)))
        .await;
    match missing {
        Err(RepoError::NotFound { reason }) => assert!(reason.contains("999"), "{reason}"),
        other => panic!("NotFound を期待したが: {other:?}"),
    }
}

/// 帳簿に1件も仕訳が無い状態でも、科目が実在すれば元帳は成功する
/// （帳簿通貨建てのゼロを返す。`LedgerParams::book_currency`）。
#[sqlx::test]
async fn a_ledger_with_no_lines_at_all_still_reports_zero_in_the_book_currency(
    pool_opts: PgPoolOptions,
    conn_opts: PgConnectOptions,
) {
    let roles = common::roles(pool_opts, conn_opts).await;
    for def in chart().iter() {
        insert_account(&roles.migrator, def).await;
    }
    let query = PgLedgerQuery::new(roles.app.clone());

    let page = query
        .ledger(&ledger_params("100", date(2026, 1, 1), date(2026, 12, 31)))
        .await
        .unwrap();

    assert!(page.rows.is_empty());
    assert_eq!(page.opening_balance.currency(), Currency::JPY);
    assert_eq!(page.closing_balance.minor(), 0);
    assert_eq!(page.account_name, "現金");
}
