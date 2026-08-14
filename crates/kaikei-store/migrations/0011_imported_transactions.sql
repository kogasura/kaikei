-- 0011_imported_transactions.sql
--
-- 明細の取込（docs/05-csv-import.md §3）。
--
-- ■ このテーブルは append-only にしない
--
-- 帳簿本体（0003 / 0004）・監査ログ（0009）・証憑（0010）は追記のみだが、
-- **ここは UPDATE を許す**。取り込んだ明細は「未処理 → 仕訳済み」と状態が
-- 変わるものであり、訂正を逆仕訳で表す帳簿とは性質が違う（§2）。
--
-- **この分離があるから帳簿の不変性を守れる。** 取込を帳簿と同じ append-only に
-- すると、状態が変わるたびに行が増えて「今どうなっているか」が読めなくなる。
-- 逆に帳簿を UPDATE 可にすれば、訂正の履歴が消える。分けることで両立する。
--
-- ■ 取込データは仕訳ではない
--
-- 列の語彙が帳簿と違う（借方/貸方ではなく入金/出金、勘定科目ではなく摘要）。
-- これは意図的である（§1）。翻訳は kaikei-app の journalize が担う。
--
-- ■ 冪等性
--
-- UNIQUE (source, external_key) が「同じ明細を2回取り込んでも重複しない」を
-- 担保する。external_key の作り方は kaikei-import の `external_key`
-- （取引後残高を含める。同日同額同摘要を区別するため）。

CREATE TABLE imported_transactions (
    id              UUID PRIMARY KEY,
    -- どの口座・カードから取り込んだか。**空を許さない**——空だと別々の口座の
    -- 明細が同じ external_key で衝突しうる。
    source          TEXT NOT NULL,
    external_key    TEXT NOT NULL,
    occurred_on     DATE NOT NULL,
    -- **常に正**。向きは direction が表す（§2）。
    amount_minor    BIGINT NOT NULL,
    currency        CHAR(3) NOT NULL DEFAULT 'JPY',
    -- 1=入金 / 2=出金。**借方/貸方ではない。**
    direction       SMALLINT NOT NULL,
    raw_description TEXT NOT NULL,
    balance_after   BIGINT,
    -- 元の CSV 行そのもの。**捨てない**——解釈を間違えたと後で分かったとき、
    -- 元が無ければ直せない。
    raw_row         JSONB NOT NULL,
    status          TEXT NOT NULL,
    -- 仕訳化したときの仕訳ID。帳簿への参照はここだけ（取込→帳簿の一方向）。
    entry_id        UUID REFERENCES journal_entries(id),
    ignore_reason   TEXT,
    imported_at     TIMESTAMPTZ NOT NULL,

    -- ★冪等性★ 同じ出どころの同じ明細は1行だけ。
    UNIQUE (source, external_key),

    CONSTRAINT imported_amount_is_positive
        CHECK (amount_minor > 0),
    CONSTRAINT imported_direction_known
        CHECK (direction IN (1, 2)),
    CONSTRAINT imported_status_known
        CHECK (status IN ('pending', 'journalized', 'ignored')),
    CONSTRAINT imported_source_not_blank
        CHECK (length(btrim(source)) > 0),
    CONSTRAINT imported_external_key_not_blank
        CHECK (length(btrim(external_key)) > 0),
    -- **状態と付随する値が食い違わないようにする。**
    -- 「仕訳済みなのに仕訳IDが無い」行があると、帳簿へ辿れないまま
    -- 「処理済み」として一覧から消える。
    CONSTRAINT imported_journalized_has_entry
        CHECK (status <> 'journalized' OR entry_id IS NOT NULL),
    CONSTRAINT imported_ignored_has_reason
        CHECK (status <> 'ignored' OR length(btrim(coalesce(ignore_reason, ''))) > 0),
    -- 未処理なら、仕訳IDも無視の理由も無い。
    CONSTRAINT imported_pending_is_clean
        CHECK (status <> 'pending' OR (entry_id IS NULL AND ignore_reason IS NULL))
);

-- 未処理の明細を日付順に引く（一覧の主な用途）。
CREATE INDEX idx_imported_status ON imported_transactions (status, occurred_on);
CREATE INDEX idx_imported_date   ON imported_transactions (occurred_on);

-- **UPDATE を許す。** 帳簿とは違い、状態が変わるテーブルである
-- （このファイル冒頭の注記を参照）。DELETE は与えない——取り込んだ記録を
-- 消せると、何を取り込んだかが追えなくなる。
GRANT SELECT, INSERT, UPDATE ON imported_transactions TO kaikei_app;
REVOKE DELETE, TRUNCATE ON imported_transactions FROM kaikei_app;
