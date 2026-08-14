//! 証憑ファイルの内容ハッシュ（[`BlobHash`]）。
//!
//! `docs/06-documents.md` §1・§2。
//!
//! # なぜ内容のハッシュで管理するのか
//!
//! ファイル名ではなく **SHA-256 で管理する**。
//!
//! - **改変が自動で見つかる。** 1 バイト変えればハッシュが変わり、帳簿の
//!   記録と一致しなくなる。真実性の担保が仕組みから出てくる
//! - **同じ証憑を2回入れても1つ。** 内容が同じなら同じハッシュになる
//! - **ファイル名の問題から解放される。** 日本語名、macOS の NFD 正規化、
//!   長さ制限をまとめて回避できる
//!
//! # この型は計算しない
//!
//! `kaikei-core` は外部 crate に依存しない層なので、**ここでは SHA-256 を
//! 計算しない**。ハッシュ値を受け取って持ち運ぶだけである。計算は
//! `kaikei-blob`（`sha2` を使える層）が行う。
//!
//! そのぶん、この型は**32バイトであること**と**16進表記の往復**を保証する。

use crate::error::CoreError;

/// 証憑ファイルの内容ハッシュ（SHA-256）。
///
/// 32バイトであることが構築時に保証される。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlobHash([u8; 32]);

impl BlobHash {
    /// 32バイトのハッシュ値から作る。
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        BlobHash(bytes)
    }

    /// 生のバイト列。
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// 16進表記（小文字・64文字）。
    pub fn to_hex(&self) -> String {
        let mut out = String::with_capacity(64);
        for byte in &self.0 {
            // `format!` を1バイトごとに呼ぶより速く、依存も増やさない。
            out.push(char::from_digit((byte >> 4) as u32, 16).expect("0..=15 は必ず16進の桁"));
            out.push(char::from_digit((byte & 0x0f) as u32, 16).expect("0..=15 は必ず16進の桁"));
        }
        out
    }

    /// 16進表記から作る。
    ///
    /// **大文字も受け付けるが、[`Self::to_hex`] は小文字で返す。** 保存先の
    /// パスやDBの値が大小で食い違わないよう、表記は1つに寄せる。
    ///
    /// # Errors
    ///
    /// 64文字でない、または16進でない文字が含まれる場合は
    /// [`CoreError::InvalidValue`]。
    pub fn parse_hex(text: &str) -> Result<Self, CoreError> {
        if text.len() != 64 {
            return Err(CoreError::InvalidValue {
                reason: format!(
                    "SHA-256 の16進表記は64文字です（受け取った長さ: {}）",
                    text.len()
                ),
            });
        }
        let mut bytes = [0u8; 32];
        let raw = text.as_bytes();
        for (index, slot) in bytes.iter_mut().enumerate() {
            let high = hex_digit(raw[index * 2])?;
            let low = hex_digit(raw[index * 2 + 1])?;
            *slot = (high << 4) | low;
        }
        Ok(BlobHash(bytes))
    }

    /// 保存先のパス表現（`3f/3fa8b2c1...`）。
    ///
    /// 先頭2文字で枝分かれさせる。**1つのディレクトリにファイルを何万も
    /// 置かない**ため（ファイルシステムによっては一覧が実用的な速度でなくなる）。
    pub fn to_path(&self) -> String {
        let hex = self.to_hex();
        format!("{}/{}", &hex[0..2], hex)
    }
}

fn hex_digit(byte: u8) -> Result<u8, CoreError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        other => Err(CoreError::InvalidValue {
            reason: format!(
                "SHA-256 の16進表記に使えない文字です: {:?}",
                char::from(other)
            ),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> BlobHash {
        let mut bytes = [0u8; 32];
        bytes[0] = 0x3f;
        bytes[1] = 0xa8;
        bytes[31] = 0xff;
        BlobHash::from_bytes(bytes)
    }

    // BH-1: 16進表記は小文字64文字。
    #[test]
    fn the_hex_form_is_lowercase_and_sixty_four_characters() {
        let hex = sample().to_hex();

        assert_eq!(hex.len(), 64);
        assert!(hex.starts_with("3fa8"), "{hex}");
        assert!(hex.ends_with("ff"), "{hex}");
        assert_eq!(hex, hex.to_lowercase());
    }

    // BH-2: **本命。** 16進表記を往復しても同じ値になる。
    //
    //       ここが崩れると、保存したファイルを見つけられなくなる。
    #[test]
    fn the_hex_form_round_trips() {
        let original = sample();

        let restored = BlobHash::parse_hex(&original.to_hex()).unwrap();

        assert_eq!(restored, original);
    }

    // BH-3: 大文字でも読めるが、書くときは小文字に寄せる。
    //
    //       表記が2つあると、保存先のパスやDBの値が食い違う。
    #[test]
    fn uppercase_input_is_accepted_but_normalised_to_lowercase() {
        let lower = sample().to_hex();
        let upper = lower.to_uppercase();

        let parsed = BlobHash::parse_hex(&upper).unwrap();

        assert_eq!(parsed, sample());
        assert_eq!(parsed.to_hex(), lower, "書くときは小文字");
    }

    // BH-4: 長さが違えば拒否する。
    #[test]
    fn a_hex_string_of_the_wrong_length_is_rejected() {
        for text in ["", "3fa8", &"a".repeat(63), &"a".repeat(65)] {
            let error = BlobHash::parse_hex(text).expect_err("拒否されるはず");
            assert!(format!("{error}").contains("64文字"), "{error}");
        }
    }

    // BH-5: 16進でない文字は拒否する。**黙って 0 として扱わない。**
    #[test]
    fn a_non_hex_character_is_rejected() {
        let text = format!("{}g", "a".repeat(63));

        let error = BlobHash::parse_hex(&text).expect_err("拒否されるはず");

        assert!(format!("{error}").contains("使えない文字"), "{error}");
    }

    // BH-6: 保存先は先頭2文字で枝分かれする。
    #[test]
    fn the_path_is_sharded_by_the_first_two_characters() {
        let path = sample().to_path();

        assert!(path.starts_with("3f/3fa8"), "{path}");
        assert_eq!(path.len(), 2 + 1 + 64);
    }

    // BH-7: 内容が1バイト違えばハッシュも違う（型として区別できる）。
    #[test]
    fn hashes_that_differ_by_one_byte_are_not_equal() {
        let mut other = *sample().as_bytes();
        other[15] ^= 0x01;

        assert_ne!(BlobHash::from_bytes(other), sample());
    }
}
