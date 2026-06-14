//! Mnemonic encoding for Reticulum destination hashes (port of ghostlink's
//! `translation.py`).
//!
//! A 16-byte destination hash is rendered as a 12-word phrase so an operator can
//! read or verify an address aloud over a voice radio instead of dictating 32
//! hex characters. The scheme is *human-memorable handle + error detection*, not
//! authentication: the 16-byte payload is carried verbatim and a 4-bit CRC8
//! check digit guards against a misheard word.
//!
//! Layout: `128` payload bits (the hash, MSB-first) followed by `4` check bits
//! (`crc8(hash) & 0x0F`) = `132` bits, split into exactly `12 * 11`-bit chunks,
//! each indexing the embedded 2048-word BIP-39 English list. The codec is
//! byte-compatible with ghostlink (see the cross-check test vector).

/// The 2048-word list (BIP-39 English), embedded so the encoding is stable
/// across builds. The file has no trailing newline, so `.lines()` yields exactly
/// 2048 entries. Kept private; callers go through [`encode`]/[`decode`].
const WORDLIST: &str = include_str!("english.txt");

/// Bits encoded per word: `log2(2048)`.
const BITS_PER_WORD: usize = 11;
/// Number of words in a phrase: `(128 payload + 4 check) / 11`.
const WORD_COUNT: usize = 12;

/// Why a mnemonic phrase could not be decoded back to a hash.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MnemonicError {
    /// The phrase did not contain exactly [`WORD_COUNT`] words.
    BadLength,
    /// A word was not in the wordlist (likely misheard/mistyped).
    UnknownWord,
    /// The CRC8 check digit did not match — the phrase is corrupt.
    Checksum,
}

/// The embedded wordlist as a slice (lazily split on first use is unnecessary —
/// `lines()` over a `&'static str` is cheap and this is called rarely).
fn words() -> impl Iterator<Item = &'static str> {
    WORDLIST.lines()
}

/// CRC-8/ATM: polynomial `0x07`, init `0x00`, no final XOR. Deterministic and
/// adequate as an operator error-detection digit (port of `translation.py`).
fn crc8(data: &[u8]) -> u8 {
    let mut crc: u8 = 0;
    for &b in data {
        crc ^= b;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 {
                (crc << 1) ^ 0x07
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// Encode a 16-byte destination hash as a 12-word mnemonic phrase.
pub fn encode(hash: &[u8; 16]) -> String {
    // 128 payload bits (MSB-first) + 4 check bits.
    let mut bits = Vec::with_capacity(WORD_COUNT * BITS_PER_WORD);
    for &byte in hash {
        for i in (0..8).rev() {
            bits.push((byte >> i) & 1);
        }
    }
    let check = crc8(hash) & 0x0F;
    for i in (0..4).rev() {
        bits.push((check >> i) & 1);
    }

    let list: Vec<&str> = words().collect();
    let mut out: Vec<&str> = Vec::with_capacity(WORD_COUNT);
    for chunk in bits.chunks(BITS_PER_WORD) {
        let idx = chunk.iter().fold(0usize, |acc, &b| (acc << 1) | b as usize);
        out.push(list[idx]);
    }
    out.join(" ")
}

/// Decode a 12-word mnemonic phrase back to the 16-byte hash, verifying the
/// CRC8 check digit. Whitespace and case-of-separators are tolerant; words
/// themselves must match the list exactly (the list is all-lowercase).
pub fn decode(phrase: &str) -> Result<[u8; 16], MnemonicError> {
    let tokens: Vec<&str> = phrase.split_whitespace().collect();
    if tokens.len() != WORD_COUNT {
        return Err(MnemonicError::BadLength);
    }

    let list: Vec<&str> = words().collect();
    // Rebuild the 132-bit stream from the word indices.
    let mut bits = Vec::with_capacity(WORD_COUNT * BITS_PER_WORD);
    for tok in tokens {
        let idx = list
            .iter()
            .position(|w| *w == tok)
            .ok_or(MnemonicError::UnknownWord)?;
        for i in (0..BITS_PER_WORD).rev() {
            bits.push(((idx >> i) & 1) as u8);
        }
    }

    // First 128 bits are the payload; the next 4 are the CRC8 low nibble.
    let mut hash = [0u8; 16];
    for (byte_idx, byte) in hash.iter_mut().enumerate() {
        let mut v = 0u8;
        for bit in 0..8 {
            v = (v << 1) | bits[byte_idx * 8 + bit];
        }
        *byte = v;
    }
    let check = bits[128..132].iter().fold(0u8, |acc, &b| (acc << 1) | b);

    if check != (crc8(&hash) & 0x0F) {
        return Err(MnemonicError::Checksum);
    }
    Ok(hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 16-byte hash for the cross-compatibility vector (matches ghostlink's
    /// codec output exactly).
    const VECTOR_HASH: [u8; 16] = [
        0xa1, 0xb2, 0xc3, 0xd4, 0xe5, 0xf6, 0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88,
        0x99,
    ];
    const VECTOR_PHRASE: &str =
        "payment noodle vivid slogan gas ancient match hammer fever crisp timber crazy";

    #[test]
    fn wordlist_is_2048_unique_words() {
        let list: Vec<&str> = words().collect();
        assert_eq!(list.len(), 2048, "wordlist must be exactly 2048 words");
        let mut sorted = list.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 2048, "wordlist must have no duplicates");
    }

    #[test]
    fn matches_ghostlink_vector() {
        // Proves byte-for-byte compatibility with the Python implementation,
        // which transitively pins the embedded wordlist content + order.
        assert_eq!(encode(&VECTOR_HASH), VECTOR_PHRASE);
        assert_eq!(decode(VECTOR_PHRASE).unwrap(), VECTOR_HASH);
    }

    #[test]
    fn zero_hash_is_all_abandon() {
        let phrase = encode(&[0u8; 16]);
        assert_eq!(phrase, ["abandon"; 12].join(" "));
        assert_eq!(decode(&phrase).unwrap(), [0u8; 16]);
    }

    #[test]
    fn round_trips_arbitrary_hash() {
        let h = [
            0x0f, 0x1e, 0x2d, 0x3c, 0x4b, 0x5a, 0x69, 0x78, 0x87, 0x96, 0xa5, 0xb4, 0xc3, 0xd2,
            0xe1, 0xf0,
        ];
        assert_eq!(decode(&encode(&h)).unwrap(), h);
    }

    #[test]
    fn produces_twelve_words() {
        assert_eq!(encode(&VECTOR_HASH).split_whitespace().count(), WORD_COUNT);
    }

    #[test]
    fn swapped_word_fails_checksum() {
        // Swap two adjacent words: still 12 known words, but the payload (and so
        // the CRC) changes.
        let mut w: Vec<&str> = VECTOR_PHRASE.split_whitespace().collect();
        w.swap(0, 1);
        assert_eq!(decode(&w.join(" ")), Err(MnemonicError::Checksum));
    }

    #[test]
    fn unknown_word_is_rejected() {
        let mut w: Vec<&str> = VECTOR_PHRASE.split_whitespace().collect();
        w[0] = "zzzznotaword";
        assert_eq!(decode(&w.join(" ")), Err(MnemonicError::UnknownWord));
    }

    #[test]
    fn wrong_word_count_is_rejected() {
        assert_eq!(decode("abandon abandon"), Err(MnemonicError::BadLength));
        assert_eq!(decode(""), Err(MnemonicError::BadLength));
    }

    #[test]
    fn tolerates_extra_whitespace() {
        let messy = format!("  {}  ", VECTOR_PHRASE.replace(' ', "   "));
        assert_eq!(decode(&messy).unwrap(), VECTOR_HASH);
    }
}
