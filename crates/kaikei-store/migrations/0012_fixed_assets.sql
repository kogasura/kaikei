-- 0012_fixed_assets.sql
--
-- 固定資産台帳（`DECISIONS.md` D-103）。
--
-- ■ 何を持つか
--
-- 償却額の計算に要る入力だけを持つ。**耐用年数も償却方法も、人が決めて
-- ここに入れる値である**（このソフトは推定しない。D-103）。
--
-- ■ append-only にしない
--
-- 帳簿本体（0003 / 0004）とは違い UPDATE を許す。台帳は「今この資産をどう
-- 償却しているか」を表すもので、耐用年数の見直しや事業専用割合の変更が
-- 起きる。訂正を逆仕訳で表す帳簿とは性質が違う（0011 と同じ理由）。
--
-- ただし **DELETE は与えない**。資産を帳簿から外すのは除却（disposed_on を
-- 埋める）であって、台帳から消すことではない。消せると、過去の年度の
-- 償却費がどの資産のものだったか辿れなくなる。
--
-- ■ 償却費の仕訳はここに持たない
--
-- 記帳したかどうかは帳簿（journal_entries）が持つ。台帳に「計上済み」
-- フラグを置くと、帳簿と台帳の2箇所が同じ事実を持つことになり、必ずずれる。

CREATE TABLE fixed_assets (
    id              UUID PRIMARY KEY,
    -- 決算書の「減価償却費の計算」欄に出す名前。
    name            TEXT NOT NULL,
    -- 帳簿上どの科目に載っているか（機械装置・工具器具備品・車両運搬具など）。
    account_code    TEXT NOT NULL REFERENCES accounts(code),
    acquired_on     DATE NOT NULL,
    -- 取得価額。**常に正。**
    acquisition_cost BIGINT NOT NULL,
    currency        CHAR(3) NOT NULL DEFAULT 'JPY',
    -- 最小単位の小数桁（journal_lines と同じ持ち方。日本円は0）。
    currency_minor_unit SMALLINT NOT NULL DEFAULT 0,
    -- 1=定額法 / 2=一括償却資産 / 3=少額減価償却資産
    -- `kaikei_jp::depreciation::DepreciationMethod` と対応する。
    method          SMALLINT NOT NULL,
    -- 耐用年数（年）。**定額法のときだけ意味がある。**
    useful_life_years SMALLINT,
    -- 事業専用割合。NULL は 100%。0 < ratio <= 1。
    business_ratio  NUMERIC(5, 4),
    -- 除却・売却した日。埋まっていればその年以降は償却しない。
    disposed_on     DATE,
    note            TEXT,

    -- **定額法には耐用年数が要る。** 無いまま入れると計算時に落ちる。
    CONSTRAINT fixed_assets_straight_line_needs_life
        CHECK (method <> 1 OR useful_life_years IS NOT NULL),
    -- 一括償却・少額特例は耐用年数を使わない。**入っていたら誤解の元**
    -- （入れた本人は効くと思っている）。
    CONSTRAINT fixed_assets_other_methods_take_no_life
        CHECK (method = 1 OR useful_life_years IS NULL),
    CONSTRAINT fixed_assets_known_method
        CHECK (method IN (1, 2, 3)),
    CONSTRAINT fixed_assets_positive_cost
        CHECK (acquisition_cost > 0),
    CONSTRAINT fixed_assets_positive_life
        CHECK (useful_life_years IS NULL OR useful_life_years > 0),
    CONSTRAINT fixed_assets_ratio_range
        CHECK (business_ratio IS NULL OR (business_ratio > 0 AND business_ratio <= 1)),
    -- 除却が取得より前にはならない。
    CONSTRAINT fixed_assets_disposed_after_acquired
        CHECK (disposed_on IS NULL OR disposed_on >= acquired_on)
);

-- 年度の償却費を出すときは「その年に生きている資産」を引く。
CREATE INDEX fixed_assets_by_acquired ON fixed_assets (acquired_on);

GRANT SELECT, INSERT, UPDATE ON fixed_assets TO kaikei_app;
REVOKE DELETE, TRUNCATE ON fixed_assets FROM kaikei_app;
