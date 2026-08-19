//! ユースケース: 取引先マスタの投入（外部の一覧 → `counterparties`）。
//!
//! # なぜこのユースケースが要るのか
//!
//! `counterparties` に行を入れる経路が本番コードに1つも無かった。検証帳簿
//! の取引先も **0 件**である。その結果、
//!
//! - `counterparty` タグを付けようとすると `PolicyError::UnknownCounterparty`
//!   で記帳が弾かれる
//! - 誰も取引先タグを付けないので、`JpTaxPolicy` の適格請求書の検証
//!   （取引先タグが**有る**ときにしか動かない）がすり抜ける
//! - 実際に、適格請求書が要る税区分の明細が**数百件**あるのに、相手方が
//!   1件も記録されていない状態になっていた（`DECISIONS.md` D-099）
//!
//! # 冪等性: 追加しかしない（[`import_chart`] と同じ。`DECISIONS.md` D-081）
//!
//! | 投入しようとした取引先 | 動作 |
//! |---|---|
//! | DB に無い | **追加する** |
//! | DB にあり、定義が完全に一致 | 何もしない |
//! | DB にあり、定義が異なる | **既存を残す。上書きしない**（[`ImportCounterpartiesOutput::kept_existing`] に載せる） |
//!
//! # `is_qualified` を外部の値で上書きしない
//!
//! `is_qualified` は**ユーザーが確認したという記録**である。`None`（未確認）と
//! `Some(false)`（非適格だと確認した）の区別が、`JpTaxPolicy` が記帳を拒むか
//! どうかを決めている。外部システムの「false」は多くの場合**誰も入力して
//! いないだけ**で、非適格だと確認したわけではない。
//!
//! 実際に外部の会計サービスの取引先一覧を調べたところ、
//! `invoice_registration_number` は全件 `null`、`qualified_invoice_issuer` は
//! 全件 `false` だった。これを `Some(false)` として取り込むと、**全社を
//! 「非適格だと確認済み」に仕立てることになる。** 投入側は未確認なら
//! `None` を渡すこと。
//!
//! [`import_chart`]: crate::usecase::import_chart

use crate::error::AppError;
use crate::ports::{ChartRepo, CounterpartyWriteRepo};
use kaikei_policy::Counterparty;

/// [`execute`] の結果。
///
/// `inserted.len() + unchanged + kept_existing.len()` が、投入しようとした
/// 取引先の件数に等しい。
#[derive(Debug, Clone)]
pub struct ImportCounterpartiesOutput {
    /// DB に存在しなかったため**投入した**取引先コード（入力の並び順）。
    pub inserted: Vec<String>,

    /// 実際に挿入された行数。
    ///
    /// 通常は `inserted.len()` と一致する。差分を取った後から挿入までの間に
    /// 別プロセスが同じコードを入れた場合だけ小さくなる
    /// （`ON CONFLICT DO NOTHING` なのでエラーにはならない）。
    pub inserted_rows: usize,

    /// 既存行と定義が完全に一致していた件数（何もしなかった）。
    pub unchanged: usize,

    /// 既存行と定義が異なるため**上書きせず既存を残した**取引先。
    pub kept_existing: Vec<CounterpartyDifference>,
}

impl ImportCounterpartiesOutput {
    /// 1行分の要約（日本語）。
    pub fn summary(&self) -> String {
        let base = format!(
            "取引先マスタ: 追加 {} 件 / 変更なし {} 件 / 既存を優先 {} 件",
            self.inserted_rows,
            self.unchanged,
            self.kept_existing.len()
        );
        if self.inserted_rows == self.inserted.len() {
            return base;
        }
        format!(
            "{base}（{} 件を追加しようとして {} 件が入りました。\
             差分を取ってから挿入するまでの間に、別のプロセスが同じ取引先を\
             投入したときに起きます。既存行は書き換えていません）",
            self.inserted.len(),
            self.inserted_rows,
        )
    }
}

/// 既存の取引先と、投入しようとした定義の食い違い。
#[derive(Debug, Clone)]
pub struct CounterpartyDifference {
    /// 対象の取引先コード。
    pub code: String,

    /// 食い違ったフィールド名（`name` / `invoice_registration_no` /
    /// `is_qualified_invoice_issuer`）。
    pub fields: Vec<&'static str>,

    /// DB に既にある定義（**こちらが残る**）。
    pub existing: Counterparty,

    /// 投入しようとした定義（採用されなかった）。
    pub incoming: Counterparty,
}

impl CounterpartyDifference {
    /// 「次に何をすればよいか」を判断できる形の説明（`CLAUDE.md` §11）。
    pub fn describe(&self) -> String {
        format!(
            "取引先 {} の定義が既存と異なります（相違: {}）。\
             既存の定義を残し、上書きしていません。\
             既存 [{}] / 投入しようとしたもの [{}]",
            self.code,
            self.fields.join(", "),
            summarize(&self.existing),
            summarize(&self.incoming),
        )
    }
}

fn summarize(c: &Counterparty) -> String {
    format!(
        "name={} invoice_registration_no={} is_qualified={}",
        c.name,
        c.invoice_registration_no.as_deref().unwrap_or("(なし)"),
        match c.is_qualified_invoice_issuer {
            None => "(未確認)",
            Some(true) => "適格",
            Some(false) => "非適格",
        },
    )
}

/// 取引先を `counterparties` に投入する（**追加のみ**）。
///
/// 呼び出し側は [`crate::tx::with_tx`] で包むこと（この関数は `begin` も
/// `commit` も呼ばない）。
///
/// 入力に同じコードが2回出てきた場合は**最初のものだけを見る**。2回目以降は
/// 1回目と比べ、違えば [`ImportCounterpartiesOutput::kept_existing`] に載る
/// （外部システムでは表記ゆれの重複が普通にある。実際に外部の会計サービスには
/// 「株式会社 ABC」と「株式会社ABC」が別々に登録されていた）。
///
/// # Errors
///
/// - 既存の取引先の読み込みに失敗した場合は [`AppError::Repo`]
/// - 挿入に失敗した場合は [`AppError::Repo`]
pub async fn execute<Tx>(
    tx: &mut Tx,
    incoming: &[Counterparty],
) -> Result<ImportCounterpartiesOutput, AppError>
where
    Tx: ChartRepo + CounterpartyWriteRepo,
{
    let existing = tx.load_counterparties().await?;

    let mut to_insert: Vec<Counterparty> = Vec::new();
    let mut inserted: Vec<String> = Vec::new();
    let mut unchanged = 0usize;
    let mut kept_existing: Vec<CounterpartyDifference> = Vec::new();

    for candidate in incoming {
        // **入力内の重複も既存と同じ扱いにする。** 先に採ったものを「既存」と
        // 見なして比べる。
        //
        // 挿入自体は重複があっても落ちない（`ON CONFLICT DO NOTHING` は
        // 同一文の中の重複も黙って飛ばす。実 DB で確認済み）。ここで落として
        // いるのは**件数を合わせるため**である。落とさないと `inserted` に
        // 2件積んで実際は1件しか入らず、`summary` が「別プロセスが同時に
        // 投入した」という**事実でない説明**を出す。表記ゆれの重複は
        // `kept_existing` として見せた方が、次に何をすればよいか分かる。
        let already = existing.get(&candidate.code).or_else(|| {
            to_insert
                .iter()
                .find(|pending| pending.code == candidate.code)
        });

        match already {
            None => {
                inserted.push(candidate.code.clone());
                to_insert.push(candidate.clone());
            }
            Some(current) => {
                let fields = differing_fields(current, candidate);
                if fields.is_empty() {
                    unchanged += 1;
                } else {
                    kept_existing.push(CounterpartyDifference {
                        code: candidate.code.clone(),
                        fields,
                        existing: current.clone(),
                        incoming: candidate.clone(),
                    });
                }
            }
        }
    }

    let inserted_rows = tx.insert_counterparties(&to_insert).await?;

    Ok(ImportCounterpartiesOutput {
        inserted,
        inserted_rows,
        unchanged,
        kept_existing,
    })
}

/// 2つの定義が食い違うフィールドを挙げる。
fn differing_fields(existing: &Counterparty, incoming: &Counterparty) -> Vec<&'static str> {
    let mut fields = Vec::new();
    if existing.name != incoming.name {
        fields.push("name");
    }
    if existing.invoice_registration_no != incoming.invoice_registration_no {
        fields.push("invoice_registration_no");
    }
    if existing.is_qualified_invoice_issuer != incoming.is_qualified_invoice_issuer {
        fields.push("is_qualified_invoice_issuer");
    }
    fields
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::InMemoryStore;
    use crate::tx::with_tx;
    use kaikei_policy::CounterpartyIndex;

    fn cp(code: &str, name: &str, qualified: Option<bool>) -> Counterparty {
        Counterparty {
            code: code.to_string(),
            name: name.to_string(),
            invoice_registration_no: None,
            is_qualified_invoice_issuer: qualified,
        }
    }

    async fn import(store: &InMemoryStore, list: &[Counterparty]) -> ImportCounterpartiesOutput {
        with_tx(store, |tx| {
            let list = list.to_vec();
            Box::pin(async move { execute(tx, &list).await })
        })
        .await
        .unwrap()
    }

    // 空の DB に投入すると全件が追加される。
    #[tokio::test]
    async fn every_counterparty_is_inserted_into_an_empty_database() {
        let store = InMemoryStore::new();
        let out = import(&store, &[cp("anthropic", "Anthropic", None)]).await;

        assert_eq!(out.inserted, vec!["anthropic".to_string()]);
        assert_eq!(out.inserted_rows, 1);
        assert_eq!(out.unchanged, 0);
        assert!(out.kept_existing.is_empty());
    }

    // 2回流しても増えない（冪等）。
    #[tokio::test]
    async fn importing_twice_does_not_add_the_same_counterparty_again() {
        let store = InMemoryStore::new();
        let list = vec![cp("anthropic", "Anthropic", None)];
        import(&store, &list).await;
        let out = import(&store, &list).await;

        assert_eq!(out.inserted_rows, 0);
        assert_eq!(out.unchanged, 1);
    }

    // **本命。** 既存の `is_qualified` を外部の値で上書きしない。
    //
    // 「適格だと確認した」取引先に、外部システムの「誰も入力していないので
    // false」が流れ込むと、確認結果が消える。
    #[tokio::test]
    async fn an_existing_qualified_flag_is_not_overwritten() {
        let store = InMemoryStore::new();
        store.set_counterparties(CounterpartyIndex::new(vec![cp(
            "anthropic",
            "Anthropic",
            Some(true),
        )]));

        let out = import(&store, &[cp("anthropic", "Anthropic", Some(false))]).await;

        assert_eq!(out.inserted_rows, 0, "上書きしない");
        assert_eq!(out.unchanged, 0);
        assert_eq!(out.kept_existing.len(), 1);
        let difference = &out.kept_existing[0];
        assert_eq!(difference.fields, vec!["is_qualified_invoice_issuer"]);
        assert_eq!(
            difference.existing.is_qualified_invoice_issuer,
            Some(true),
            "残るのは既存の確認結果"
        );
        assert!(
            difference.describe().contains("適格"),
            "既存の値を見せること: {}",
            difference.describe()
        );
    }

    // 入力の中に同じコードが2回あっても壊れない。
    //
    // 外部システムでは表記ゆれの重複が普通にある（外部の会計サービスには
    // 「株式会社 ABC」と「株式会社ABC」が別に登録されていた）。
    #[tokio::test]
    async fn a_duplicate_code_within_the_input_is_reported_not_inserted_twice() {
        let store = InMemoryStore::new();
        let out = import(
            &store,
            &[
                cp("abc", "株式会社ABC", None),
                cp("abc", "株式会社 ABC", None),
            ],
        )
        .await;

        assert_eq!(out.inserted_rows, 1, "1件しか入らない");
        assert_eq!(out.kept_existing.len(), 1, "2件目は食い違いとして返す");
        assert_eq!(out.kept_existing[0].fields, vec!["name"]);
    }

    // 名前だけが違う場合も既存を残す。
    #[tokio::test]
    async fn a_renamed_counterparty_keeps_the_existing_name() {
        let store = InMemoryStore::new();
        store.set_counterparties(CounterpartyIndex::new(vec![cp("abc", "ABC", None)]));

        let out = import(&store, &[cp("abc", "株式会社ABC", None)]).await;

        assert_eq!(out.inserted_rows, 0);
        assert_eq!(out.kept_existing.len(), 1);
        assert_eq!(out.kept_existing[0].existing.name, "ABC");
    }
}
