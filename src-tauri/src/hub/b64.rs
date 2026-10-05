// Hand-rolled strict RFC 4648 base64 decoder (standard alphabet, no URL-safe
// variant, no new dependency). Used by the hub inbound-file pipeline to
// decode relay payloads; strictness is a security property here: invalid
// characters, bad padding and non-canonical trailing bits are all rejected.

/// Decoder failure modes; the caller maps them to a generic "not a file
/// payload" drop, so the variants exist for tests and logs only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum B64Error {
    /// Character outside the standard alphabet (or '=' mid-string).
    InvalidChar,
    /// Input length is not a multiple of 4.
    InvalidLength,
    /// More than two '=' or non-zero trailing bits (non-canonical).
    InvalidPadding,
}

pub fn encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let n = (chunk[0] as u32) << 16
            | (*chunk.get(1).unwrap_or(&0) as u32) << 8
            | *chunk.get(2).unwrap_or(&0) as u32;
        out.push(TABLE[(n >> 18 & 63) as usize] as char);
        out.push(TABLE[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6 & 63) as usize]
        } else {
            b'='
        } as char);
        out.push(if chunk.len() > 2 {
            TABLE[(n & 63) as usize]
        } else {
            b'='
        } as char);
    }
    out
}

pub fn decode(input: &str) -> Result<Vec<u8>, B64Error> {
    let bytes = input.as_bytes();
    if bytes.len() % 4 != 0 {
        return Err(B64Error::InvalidLength);
    }
    // Padding is valid only as 1-2 trailing '=' characters.
    let pad = bytes.iter().rev().take_while(|&&b| b == b'=').count();
    if pad > 2 {
        return Err(B64Error::InvalidPadding);
    }
    let data_len = bytes.len() - pad;
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    let mut acc: u32 = 0;
    let mut nbits = 0u32;
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'=' {
            if i < data_len {
                return Err(B64Error::InvalidChar); // '=' before the trailing pad
            }
            continue;
        }
        let v = match b {
            b'A'..=b'Z' => (b - b'A') as u32,
            b'a'..=b'z' => (b - b'a' + 26) as u32,
            b'0'..=b'9' => (b - b'0' + 52) as u32,
            b'+' => 62,
            b'/' => 63,
            _ => return Err(B64Error::InvalidChar),
        };
        acc = (acc << 6) | v;
        nbits += 6;
        if nbits >= 8 {
            nbits -= 8;
            out.push((acc >> nbits) as u8);
        }
    }
    // Strict: the bits left over by the final quantum must be zero
    // (canonical encoding), never payload smuggled past the length check.
    if nbits > 0 && (acc & ((1 << nbits) - 1)) != 0 {
        return Err(B64Error::InvalidPadding);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(input: &str) -> Vec<u8> {
        decode(input).unwrap()
    }

    #[test]
    fn decode_matches_known_vectors_including_padding() {
        assert_eq!(ok(""), b"");
        assert_eq!(ok("Zg=="), b"f");
        assert_eq!(ok("Zm8="), b"fo");
        assert_eq!(ok("Zm9v"), b"foo");
        assert_eq!(ok("TWFu"), b"Man");
        assert_eq!(ok("QQ=="), b"A");
        assert_eq!(ok("SGVsbG8sIHdvcmxkIQ=="), b"Hello, world!");
        assert_eq!(
            ok("aG9sYQ=="),
            b"hola",
            "the integration-test file payload must decode"
        );
    }

    #[test]
    fn decode_rejects_characters_outside_the_standard_alphabet() {
        assert_eq!(decode("QQ=Q"), Err(B64Error::InvalidChar), "'=' mid-string");
        assert_eq!(decode("AB C"), Err(B64Error::InvalidChar), "space");
        assert_eq!(decode("AB-C"), Err(B64Error::InvalidChar), "url-safe '-'");
        assert_eq!(decode("AB_C"), Err(B64Error::InvalidChar), "url-safe '_'");
        assert_eq!(decode("AB*C"), Err(B64Error::InvalidChar), "symbol");
    }

    #[test]
    fn decode_rejects_lengths_that_are_not_a_multiple_of_four() {
        assert_eq!(decode("Q"), Err(B64Error::InvalidLength));
        assert_eq!(decode("QQQ"), Err(B64Error::InvalidLength));
        assert_eq!(decode("QQQQQ"), Err(B64Error::InvalidLength));
    }

    #[test]
    fn decode_rejects_misplaced_and_excess_padding() {
        assert_eq!(decode("A==="), Err(B64Error::InvalidPadding));
        assert_eq!(decode("===="), Err(B64Error::InvalidPadding));
        assert_eq!(decode("==QQ"), Err(B64Error::InvalidChar));
        assert_eq!(decode("AB=C"), Err(B64Error::InvalidChar));
    }

    #[test]
    fn decode_rejects_non_canonical_trailing_bits() {
        assert_eq!(decode("QR=="), Err(B64Error::InvalidPadding));
        assert_eq!(decode("QRV="), Err(B64Error::InvalidPadding));
    }

    #[test]
    fn encode_matches_rfc4648_vectors_with_canonical_padding() {
        assert_eq!(encode(b""), "");
        assert_eq!(encode(b"M"), "TQ==");
        assert_eq!(encode(b"Ma"), "TWE=");
        assert_eq!(encode(b"Man"), "TWFu");
        assert_eq!(encode(b"f"), "Zg==");
        assert_eq!(encode(b"fo"), "Zm8=");
        assert_eq!(encode(b"foo"), "Zm9v");
        assert_eq!(encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn encode_roundtrips_with_the_strict_decoder() {
        for len in 0..=24usize {
            let bytes: Vec<u8> = (0..len).map(|i| (i * 37 + 11 % 251) as u8).collect();
            let encoded = encode(&bytes);
            assert_eq!(
                encoded.len() % 4,
                0,
                "output must always be padded to a 4-char quantum (len {len})"
            );
            assert_eq!(
                decode(&encoded),
                Ok(bytes),
                "roundtrip must reproduce the original bytes (len {len})"
            );
        }
    }
}
