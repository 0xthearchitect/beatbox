//! Numbers nobody should be able to guess twice, with no crate behind them.
//!
//! A resync token and a machine's SMBIOS UUID need to be unlikely to repeat,
//! not secret. The standard library's randomly keyed hasher, fed the time and
//! a counter, is enough for both.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// 64 bits nobody chose.
pub(crate) fn next() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut hasher = RandomState::new().build_hasher();
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    hasher.write_u128(since.as_nanos());
    hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    hasher.finish()
}

/// A version 4 UUID, lowercase with dashes.
pub fn uuid4() -> String {
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&next().to_le_bytes());
    bytes[8..].copy_from_slice(&next().to_le_bytes());
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_uuid_is_a_version_four_one_and_never_the_null_one() {
        let made = uuid4();
        assert_eq!(made.len(), 36);
        assert_eq!(&made[14..15], "4");
        assert!("89ab".contains(&made[19..20]));
        assert_ne!(made, "00000000-0000-0000-0000-000000000000");
        assert_ne!(uuid4(), uuid4());
    }
}
