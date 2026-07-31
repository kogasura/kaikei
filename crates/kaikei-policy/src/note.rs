//! policy が処理結果に添付する注記（`PolicyNote`）。
//!
//! `CLAUDE.md` §10 は「提案系の機能は候補と根拠を返し、確定は人間に残す」
//! ことと「税務判断を断定するメッセージを出さない」ことを求める。
//! `TaxDerivation` 等の戻り値にこの注記を添えることで、
//! 「非適格の経過措置がある」「適用可否は税理士に確認してほしい」といった
//! 情報を、断定ではなく候補・根拠として利用者に返せるようにする。

/// 注記の重要度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteSeverity {
    /// 参考情報。処理結果の理解を助けるための補足。
    Info,
    /// 人間の確認を推奨する注意。断定はしないが見落としてほしくない情報。
    Warning,
}

/// policy が処理結果に添付する注記。
///
/// 税務判断を断定する文言（例: 「この経費は損金です」）は書かない
/// （`CLAUDE.md` §10）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyNote {
    /// 重要度。
    pub severity: NoteSeverity,
    /// 注記の本文。
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_note_holds_severity_and_message() {
        let note = PolicyNote {
            severity: NoteSeverity::Warning,
            message: "適用可否は税理士にご確認ください".to_string(),
        };
        assert_eq!(note.severity, NoteSeverity::Warning);
        assert_eq!(note.message, "適用可否は税理士にご確認ください");
    }
}
