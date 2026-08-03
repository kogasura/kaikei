//! ユースケース: 勘定科目マスタの投入（テンプレート → `accounts`）。
//!
//! # なぜこのユースケースが要るのか
//!
//! `post_entry::execute` は `tx.load_chart()` で **DB の `accounts`** を読む。
//! `kaikei_jp::compose::compose` が返す `chart`（埋め込みテンプレート由来）を
//! コード上で持っているだけでは1件も記帳できない。PR-E 以前、本番コードに
//! `INSERT INTO accounts` は1箇所も無く、投入経路は E2E テストのフィクスチャ
//! （`crates/kaikei-e2e/tests/e2e_jp.rs` の `seed_chart`。migrator ロールで
//! 生 SQL を発行していた）にしか存在しなかった（`DECISIONS.md` D-070 の決定2）。
//!
//! MCP ツールにはしない（同 D-070）。合成ルートが起動時に呼ぶ
//! ユースケースとして `kaikei-app` に置く。
//!
//! # 冪等性: 追加しかしない（`DECISIONS.md` D-081）
//!
//! | 投入しようとした科目 | 動作 |
//! |---|---|
//! | DB に無い | **追加する** |
//! | DB にあり、定義が完全に一致 | 何もしない |
//! | DB にあり、定義が異なる | **既存を残す。上書きしない**（[`ImportChartOutput::kept_existing`] に載せて呼び出し側へ返す） |
//!
//! `accounts` は帳簿本体と違って append-only ではなく、DB 権限としては
//! `UPDATE` が許可されている（`0002_accounts.sql`）。**それでもこの経路は
//! 追加しか行わない。** 既に仕訳が参照している科目の名称・種別を起動のたびに
//! テンプレートで上書きすると、
//!
//! - ユーザーが編集した科目名が起動のたびに消える
//!   （`DECISIONS.md` D-069 は「ユーザーが科目名を編集する」ことを前提にしている）
//! - 科目種別（`account_type`）が変われば、**過去の仕訳の意味が後から変わる**
//!   （試算表の残高の符号、決算書の区分）
//!
//! という2つの実害が出る。差異は握り潰さず [`ChartDifference`] として返し、
//! 合成ルートが起動時に stderr へ出す（`docs/07-mcp-server.md` §4。
//! stdout は JSON-RPC 専用チャネル）。
//!
//! # このユースケースがしないこと
//!
//! - 科目の**更新・削除・無効化**（`active` 列は触らない）
//! - `sort_order` の設定（テンプレートの `sort` は `kaikei_core::AccountDef` に
//!   対応するフィールドが無く、ロード時に破棄されている。`DECISIONS.md` D-061）
//! - 取引先マスタの投入（Phase 4 以降）

use crate::error::AppError;
use crate::ports::{ChartRepo, ChartWriteRepo};
use crate::wire::account_type_code;
use kaikei_core::{AccountCode, AccountDef, ChartOfAccounts};

/// [`execute`] の結果。
///
/// 3つのフィールドの件数の合計は、投入しようとしたテンプレートの科目数に等しい
/// （どの科目も必ず「追加した」「一致していた」「異なるので既存を残した」の
/// いずれか1つに分類される）。
#[derive(Debug, Clone)]
pub struct ImportChartOutput {
    /// DB に存在しなかったため**投入した**科目コード（テンプレートの並び順）。
    pub inserted: Vec<AccountCode>,

    /// 実際に挿入された行数。
    ///
    /// 通常は `inserted.len()` と一致する。**一致しないのは、差分を取った後
    /// から挿入までの間に別プロセス（同時に起動したもう1つのサーバ等）が
    /// 同じ科目を入れた場合**で、実装は `ON CONFLICT DO NOTHING` 相当なので
    /// エラーにはならず、この数だけが小さくなる。
    pub inserted_rows: usize,

    /// 既存行と定義が完全に一致していた科目の件数（何もしなかった）。
    pub unchanged: usize,

    /// 既存行と定義が異なるため**上書きせず既存を残した**科目。
    ///
    /// 空でないことはエラーではない（ユーザーが科目名を編集した後の
    /// 通常の起動でも起きる）。呼び出し側は診断として出力すること。
    pub kept_existing: Vec<ChartDifference>,
}

impl ImportChartOutput {
    /// 起動ログ1行分の要約（日本語）。**stdout ではなく stderr に出すこと**
    /// （`docs/07-mcp-server.md` §4）。
    pub fn summary(&self) -> String {
        format!(
            "勘定科目マスタ: 追加 {} 件 / 変更なし {} 件 / 既存を優先 {} 件",
            self.inserted_rows,
            self.unchanged,
            self.kept_existing.len()
        )
    }
}

/// 既存の科目定義と、投入しようとした定義の食い違い。
#[derive(Debug, Clone)]
pub struct ChartDifference {
    /// 対象の科目コード。
    pub code: AccountCode,

    /// 食い違ったフィールド名（`name` / `account_type` / `parent` / `postable`）。
    ///
    /// 線上（応答 JSON）に出る語彙ではなく**診断用の識別子**だが、
    /// `account_type` の値は [`crate::wire::account_type_code`] を通す
    /// （同じ列挙型の綴りを2箇所で決めない。`DECISIONS.md` D-072）。
    pub fields: Vec<&'static str>,

    /// DB に既にある定義（**こちらが残る**）。
    pub existing: AccountDef,

    /// 投入しようとした定義（採用されなかった）。
    pub incoming: AccountDef,
}

impl ChartDifference {
    /// 人間・AI が「次に何をすればよいか」を判断できる形の説明（`CLAUDE.md` §11）。
    pub fn describe(&self) -> String {
        format!(
            "科目 {} の定義が既存と異なります（相違: {}）。\
             既存の定義を残し、テンプレートでは上書きしていません。\
             既存 [{}] / テンプレート [{}]。\
             テンプレート側を採用したい場合は、勘定科目マスタを直接編集してください",
            self.code.as_str(),
            self.fields.join(", "),
            summarize(&self.existing),
            summarize(&self.incoming),
        )
    }
}

fn summarize(def: &AccountDef) -> String {
    format!(
        "name={} account_type={} parent={} postable={}",
        def.name,
        account_type_code(def.account_type),
        def.parent
            .as_ref()
            .map(|p| p.as_str().to_string())
            .unwrap_or_else(|| "(なし)".to_string()),
        def.postable,
    )
}

/// テンプレートの勘定科目を `accounts` に投入する（**追加のみ**）。
///
/// 呼び出し側は [`crate::tx::with_tx`] で包むこと（この関数は `begin` も
/// `commit` も呼ばない）。
///
/// # Errors
///
/// - 既存の勘定科目表の読み込みに失敗した場合は [`AppError::Repo`]
/// - 挿入に失敗した場合（親科目が存在しない等）は [`AppError::Repo`]
pub async fn execute<Tx>(
    tx: &mut Tx,
    template: &ChartOfAccounts,
) -> Result<ImportChartOutput, AppError>
where
    Tx: ChartRepo + ChartWriteRepo,
{
    let existing = tx.load_chart().await?;

    let mut to_insert: Vec<AccountDef> = Vec::new();
    let mut unchanged = 0usize;
    let mut kept_existing: Vec<ChartDifference> = Vec::new();

    for incoming in template.iter() {
        match existing.get(&incoming.code) {
            None => to_insert.push(incoming.clone()),
            Some(current) => {
                let fields = differing_fields(current, incoming);
                if fields.is_empty() {
                    unchanged += 1;
                } else {
                    kept_existing.push(ChartDifference {
                        code: incoming.code.clone(),
                        fields,
                        existing: current.clone(),
                        incoming: incoming.clone(),
                    });
                }
            }
        }
    }

    let inserted: Vec<AccountCode> = to_insert.iter().map(|d| d.code.clone()).collect();
    let inserted_rows = if to_insert.is_empty() {
        0
    } else {
        tx.insert_accounts(&to_insert).await?
    };

    Ok(ImportChartOutput {
        inserted,
        inserted_rows,
        unchanged,
        kept_existing,
    })
}

/// 同じ科目コードを持つ2つの定義で食い違うフィールドを列挙する。
///
/// `AccountDef` は `PartialEq` を導出していない（`kaikei-core` は凍結層で、
/// 導出を足すには人間の承認が要る。`CLAUDE.md` §1）。どのみち
/// 「どのフィールドが違うか」を診断に出すのでフィールドごとに比べる。
fn differing_fields(existing: &AccountDef, incoming: &AccountDef) -> Vec<&'static str> {
    let mut fields = Vec::new();
    if existing.name != incoming.name {
        fields.push("name");
    }
    if existing.account_type != incoming.account_type {
        fields.push("account_type");
    }
    if existing.parent != incoming.parent {
        fields.push("parent");
    }
    if existing.postable != incoming.postable {
        fields.push("postable");
    }
    fields
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::InMemoryStore;
    use crate::tx::with_tx;
    use kaikei_core::AccountType;

    fn def(code: &str, name: &str, account_type: AccountType) -> AccountDef {
        AccountDef {
            code: AccountCode::parse(code).unwrap(),
            name: name.to_string(),
            account_type,
            parent: None,
            postable: true,
        }
    }

    fn template() -> ChartOfAccounts {
        ChartOfAccounts::new(vec![
            def("100", "現金", AccountType::Asset),
            def("500", "売上高", AccountType::Revenue),
        ])
        .unwrap()
    }

    async fn import(store: &InMemoryStore, chart: &ChartOfAccounts) -> ImportChartOutput {
        with_tx(store, |tx| {
            let chart = chart.clone();
            Box::pin(async move { execute(tx, &chart).await })
        })
        .await
        .unwrap()
    }

    // IC-1: 空の DB に投入すると全件が追加される。
    #[tokio::test]
    async fn import_chart_inserts_every_account_into_an_empty_database() {
        let store = InMemoryStore::new();
        let out = import(&store, &template()).await;

        assert_eq!(out.inserted.len(), 2);
        assert_eq!(out.inserted_rows, 2);
        assert_eq!(out.unchanged, 0);
        assert!(out.kept_existing.is_empty());

        let stored = with_tx(&store, |tx| {
            Box::pin(async move { Ok::<_, AppError>(tx.load_chart().await?) })
        })
        .await
        .unwrap();
        assert_eq!(stored.iter().count(), 2);
    }

    // IC-2: 2回流しても2回目は何もしない（冪等）。
    #[tokio::test]
    async fn import_chart_is_idempotent_when_run_twice() {
        let store = InMemoryStore::new();
        import(&store, &template()).await;
        let second = import(&store, &template()).await;

        assert!(second.inserted.is_empty(), "2回目に追加が起きてはいけない");
        assert_eq!(second.inserted_rows, 0);
        assert_eq!(second.unchanged, 2);
        assert!(second.kept_existing.is_empty());
    }

    // IC-3: 既存の定義と食い違う科目は上書きせず、既存が残る。
    #[tokio::test]
    async fn import_chart_keeps_the_existing_definition_when_it_differs() {
        // ユーザーが科目名を編集した後の状態を模す。
        let edited = ChartOfAccounts::new(vec![
            def("100", "現金（手許）", AccountType::Asset),
            def("500", "売上高", AccountType::Revenue),
        ])
        .unwrap();
        let store = InMemoryStore::with_chart(edited);

        let out = import(&store, &template()).await;

        assert!(out.inserted.is_empty());
        assert_eq!(out.unchanged, 1, "売上高だけが一致する");
        assert_eq!(out.kept_existing.len(), 1);
        let diff = &out.kept_existing[0];
        assert_eq!(diff.code.as_str(), "100");
        assert_eq!(diff.fields, vec!["name"]);

        // 既存の名称が保たれている（テンプレートで上書きされていない）。
        let stored = with_tx(&store, |tx| {
            Box::pin(async move { Ok::<_, AppError>(tx.load_chart().await?) })
        })
        .await
        .unwrap();
        let cash = AccountCode::parse("100").unwrap();
        assert_eq!(stored.get(&cash).unwrap().name, "現金（手許）");
    }

    // IC-4: テンプレートに増えた科目だけが追加される（差分投入）。
    #[tokio::test]
    async fn import_chart_adds_only_the_accounts_missing_from_the_database() {
        let store = InMemoryStore::new();
        import(&store, &template()).await;

        let extended = ChartOfAccounts::new(vec![
            def("100", "現金", AccountType::Asset),
            def("500", "売上高", AccountType::Revenue),
            def("520", "雑収入", AccountType::Revenue),
        ])
        .unwrap();
        let out = import(&store, &extended).await;

        assert_eq!(out.inserted.len(), 1);
        assert_eq!(out.inserted[0].as_str(), "520");
        assert_eq!(out.unchanged, 2);
    }

    // 診断の文言に、既存とテンプレートの両方の内容と次の手が含まれる（CLAUDE.md §11）。
    #[tokio::test]
    async fn chart_difference_describes_both_sides_and_the_next_step() {
        let edited = ChartOfAccounts::new(vec![def("100", "現金", AccountType::Expense)]).unwrap();
        let store = InMemoryStore::with_chart(edited);
        let out = import(
            &store,
            &ChartOfAccounts::new(vec![def("100", "現金", AccountType::Asset)]).unwrap(),
        )
        .await;

        let text = out.kept_existing[0].describe();
        assert!(text.contains("account_type"), "text = {text}");
        assert!(text.contains("expense"), "text = {text}");
        assert!(text.contains("asset"), "text = {text}");
        assert!(text.contains("上書きしていません"), "text = {text}");
        assert!(text.contains("編集"), "text = {text}");
    }

    // 空のテンプレートを投入しても壊れない（ポートを呼ばない）。
    #[tokio::test]
    async fn import_chart_with_an_empty_template_does_nothing() {
        let store = InMemoryStore::new();
        let out = import(&store, &ChartOfAccounts::new(Vec::new()).unwrap()).await;
        assert_eq!(out.inserted_rows, 0);
        assert_eq!(out.unchanged, 0);
        assert!(out.kept_existing.is_empty());
    }
}
