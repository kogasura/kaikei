//! 取引先（`Counterparty`）とその索引（`CounterpartyIndex`）。
//!
//! `TaxPolicy` が適格請求書発行事業者かどうかを判定するために必要な、
//! 最小限の取引先情報を保持する。DB の取引先マスタそのものではなく、
//! `kaikei-app` が読み込んだスナップショットを表す（`CLAUDE.md` §3）。
//!
//! 登録番号（`invoice_registration_no`）の**形式検証・チェックデジット検証**は
//! `kaikei-jp` の責務（`docs/04-jp-tax.md` §6）。ここでは検証済みの文字列を
//! そのまま保持するだけで、解釈はしない。

use std::collections::BTreeMap;

/// 取引先。
///
/// 適格請求書発行事業者としての登録状況は、ユーザーが確認して記録するまでは
/// 不明でありうるため `Option<bool>`（`docs/08-compliance.md` §6・
/// `docs/03-database.md` の `counterparties.is_qualified` が `NULL` を許す列に
/// 対応する）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Counterparty {
    /// 取引先コード。仕訳明細のタグ（`counterparty` キー）に入る値と対応する。
    pub code: String,
    /// 表示名。エラーメッセージ等に使う。
    pub name: String,
    /// 適格請求書発行事業者登録番号（`T` + 13桁）。未登録なら `None`。
    pub invoice_registration_no: Option<String>,
    /// 適格請求書発行事業者かどうか。ユーザーが確認して記録するまでは `None`
    /// （未確認）。
    pub is_qualified_invoice_issuer: Option<bool>,
}

/// 取引先の索引。`code` で引く。
///
/// `kaikei-app` が DB から読み込んだスナップショットを構築し、
/// `TaxContext::counterparties` に詰めて渡す。
#[derive(Debug, Clone, Default)]
pub struct CounterpartyIndex {
    by_code: BTreeMap<String, Counterparty>,
}

impl CounterpartyIndex {
    /// 取引先の一覧から索引を作る。同じ `code` が複数あれば後勝ちで上書きする。
    pub fn new(counterparties: Vec<Counterparty>) -> Self {
        CounterpartyIndex {
            by_code: counterparties
                .into_iter()
                .map(|c| (c.code.clone(), c))
                .collect(),
        }
    }

    /// 取引先を1件も持たない索引を作る。
    pub fn empty() -> Self {
        CounterpartyIndex::default()
    }

    /// コードから取引先を取得する。
    pub fn get(&self, code: &str) -> Option<&Counterparty> {
        self.by_code.get(code)
    }

    /// 登録されている取引先数を返す。
    pub fn len(&self) -> usize {
        self.by_code.len()
    }

    /// 取引先を1件も持たないかどうか。
    pub fn is_empty(&self) -> bool {
        self.by_code.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counterparty(code: &str, is_qualified: Option<bool>) -> Counterparty {
        Counterparty {
            code: code.to_string(),
            name: format!("取引先{code}"),
            invoice_registration_no: None,
            is_qualified_invoice_issuer: is_qualified,
        }
    }

    #[test]
    fn empty_index_has_no_entries() {
        let index = CounterpartyIndex::empty();
        assert!(index.is_empty());
        assert_eq!(index.len(), 0);
        assert!(index.get("CP0001").is_none());
    }

    #[test]
    fn new_index_looks_up_by_code() {
        let index = CounterpartyIndex::new(vec![
            counterparty("CP0001", Some(true)),
            counterparty("CP0002", None),
        ]);
        assert_eq!(index.len(), 2);
        assert_eq!(
            index.get("CP0001").unwrap().is_qualified_invoice_issuer,
            Some(true)
        );
        assert_eq!(
            index.get("CP0002").unwrap().is_qualified_invoice_issuer,
            None
        );
        assert!(index.get("CP9999").is_none());
    }

    #[test]
    fn new_index_last_entry_wins_on_duplicate_code() {
        let index = CounterpartyIndex::new(vec![
            counterparty("CP0001", Some(false)),
            counterparty("CP0001", Some(true)),
        ]);
        assert_eq!(index.len(), 1);
        assert_eq!(
            index.get("CP0001").unwrap().is_qualified_invoice_issuer,
            Some(true)
        );
    }
}
