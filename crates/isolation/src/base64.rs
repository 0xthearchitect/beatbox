//! Base64, which is how the guest agent carries bytes through JSON.
//!
//! Twenty lines rather than a crate. The decoder reads what a guest sent, so
//! it is deliberately incurious: anything that is not base64 is `None`, never
//! a panic and never a guess.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64, padded.
pub fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for index in 0..4 {
            if index <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * index) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Standard base64 back into bytes, or `None` for anything that is not.
pub fn decode(text: &str) -> Option<Vec<u8>> {
    let clean: Vec<u8> = text.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if !clean.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(clean.len() / 4 * 3);
    let last = clean.len() / 4;
    for (index, chunk) in clean.chunks(4).enumerate() {
        let padding = chunk.iter().rev().take_while(|b| **b == b'=').count();
        // Padding ends the text: only the last chunk may carry any.
        if padding > 2 || chunk[..4 - padding].contains(&b'=') || (padding > 0 && index + 1 != last)
        {
            return None;
        }
        let mut n = 0u32;
        for byte in chunk {
            let value = match byte {
                b'A'..=b'Z' => byte - b'A',
                b'a'..=b'z' => byte - b'a' + 26,
                b'0'..=b'9' => byte - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                b'=' => 0,
                _ => return None,
            };
            n = (n << 6) | u32::from(value);
        }
        let bytes = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
        out.extend_from_slice(&bytes[..3 - padding]);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rfc_4648_vectors() {
        for (plain, coded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(encode(plain.as_bytes()), coded);
            assert_eq!(decode(coded).unwrap(), plain.as_bytes());
        }
    }

    #[test]
    fn what_is_not_base64_is_nothing_rather_than_a_guess() {
        for bad in ["Zg=", "Z===", "Zm9v!", "=Zm9", "Zg==Zg=="] {
            assert!(decode(bad).is_none(), "{bad}");
        }
    }
}
