//! Where moments and ids come from, when nobody hands the runtime its own.
//!
//! Always handed in, never global, so a test gets a repeatable story. The
//! time is written the way Python's `isoformat()` wrote it, because the
//! records under `~/.beatbox/state/` were written that way and are read back.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// A moment, as text. See [`now`].
pub type Clock = Arc<dyn Fn() -> String + Send + Sync>;

/// A fresh sandbox id, as text. See [`new_id`].
pub type Ids = Arc<dyn Fn() -> String + Send + Sync>;

/// The system's clock.
pub fn system_clock() -> Clock {
    Arc::new(now)
}

/// Random ids.
pub fn random_ids() -> Ids {
    Arc::new(new_id)
}

/// Now, in UTC: `2026-09-24T14:32:07.123456+00:00`, and without the
/// fraction when it is exactly zero, as Python's `isoformat()` does.
pub fn now() -> String {
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    iso(since.as_secs(), since.subsec_micros())
}

/// `seconds` since the epoch, plus `micros`, written the way [`now`] does.
pub fn iso(seconds: u64, micros: u32) -> String {
    let days = (seconds / 86_400) as i64;
    let rest = seconds % 86_400;
    let (year, month, day) = civil(days);
    let (hour, minute, second) = (rest / 3600, rest % 3600 / 60, rest % 60);
    let fraction = if micros == 0 {
        String::new()
    } else {
        format!(".{micros:06}")
    };
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}{fraction}+00:00")
}

/// Days since 1970-01-01 as a proleptic Gregorian date (Howard Hinnant's
/// `civil_from_days`), so that no date library is needed for one format.
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// `sbx-` and twelve hex digits.
///
/// Not a secret, only unlikely to repeat (the runtime refuses a repeat), so
/// std's randomly keyed hasher fed the time and a counter is enough.
pub fn new_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut hasher = RandomState::new().build_hasher();
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    hasher.write_u128(since.as_nanos());
    hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    format!("sbx-{:012x}", hasher.finish() & 0xffff_ffff_ffff)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shape_is_the_one_python_wrote() {
        assert_eq!(iso(0, 0), "1970-01-01T00:00:00+00:00");
        // 2026-09-22T14:32:07+00:00, the engine plan's own example moment.
        assert_eq!(iso(1_790_087_527, 0), "2026-09-22T14:32:07+00:00");
        assert_eq!(iso(1_790_087_527, 42), "2026-09-22T14:32:07.000042+00:00");
        assert_eq!(iso(951_782_400, 0), "2000-02-29T00:00:00+00:00");
    }

    #[test]
    fn ids_look_like_sandbox_ids_and_do_not_repeat() {
        let made: std::collections::HashSet<String> = (0..1000).map(|_| new_id()).collect();
        assert_eq!(made.len(), 1000);
        for id in &made {
            assert_eq!(id.len(), 16);
            assert!(id.starts_with("sbx-"));
            assert!(id[4..].chars().all(|c| c.is_ascii_hexdigit()));
        }
    }
}
