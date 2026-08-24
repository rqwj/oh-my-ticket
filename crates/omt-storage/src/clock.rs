//! Injectable clocks (determinism requirement): no test may observe real
//! time. [`MillisClock`] drives lock staleness, journal stamps, and event
//! timestamps; ISO formatting mirrors `Date.prototype.toISOString` byte
//! for byte (`2026-08-24T05:00:00.000Z`) so lock bodies stay cross-language
//! identical.

use std::time::{SystemTime, UNIX_EPOCH};

/// Millisecond clock port. `Send + Sync` so the daemon (U5) can share one
/// instance across threads.
pub trait MillisClock: Send + Sync {
    fn now_ms(&self) -> i64;
}

/// Production clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl MillisClock for SystemClock {
    fn now_ms(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }
}

/// Fixed deterministic clock: starts at `start_ms`, advanced only by
/// [`FixedClock::advance`] (tests drive time explicitly).
#[derive(Debug)]
pub struct FixedClock {
    current: std::sync::atomic::AtomicI64,
}

impl FixedClock {
    pub fn at_ms(start_ms: i64) -> Self {
        FixedClock {
            current: std::sync::atomic::AtomicI64::new(start_ms),
        }
    }

    /// Move the clock forward (never backward) by `ms`.
    pub fn advance(&self, ms: i64) {
        self.current
            .fetch_add(ms, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn now(&self) -> i64 {
        self.current.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl MillisClock for FixedClock {
    fn now_ms(&self) -> i64 {
        self.now()
    }
}

/// `Date.toISOString()` equivalent for a millisecond epoch:
/// `YYYY-MM-DDTHH:MM:SS.sssZ`, proleptic Gregorian, UTC.
pub fn iso_from_ms(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    let rem = ms.rem_euclid(86_400_000);
    let (year, month, day) = civil_from_days(days);
    let hour = rem / 3_600_000;
    let minute = (rem % 3_600_000) / 60_000;
    let second = (rem % 60_000) / 1000;
    let milli = rem % 1000;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{milli:03}Z")
}

/// Parse the subset of ISO-8601 this codebase writes back to ms
/// (`...THH:MM:SS(.sss)?Z`). Returns `None` on anything else — callers then
/// fall back to mtime liveness like the TypeScript lock reader.
pub fn parse_iso_ms(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() < 19
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return None;
    }
    let num = |range: std::ops::Range<usize>| text.get(range)?.parse::<i64>().ok();
    let year = num(0..4)?;
    let month = num(5..7)?;
    let day = num(8..10)?;
    let hour = num(11..13)?;
    let minute = num(14..16)?;
    let second = num(17..19)?;
    let milli = if text.len() >= 24 && &text[19..20] == "." && text.ends_with('Z') {
        num(20..23)?
    } else {
        0
    };
    let days = days_from_civil(year, month, day);
    Some(days * 86_400_000 + hour * 3_600_000 + minute * 60_000 + second * 1000 + milli)
}

/// Howard Hinnant's `civil_from_days` (public domain formulation).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Howard Hinnant's `days_from_civil`.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(non_snake_case)] // pins the JS toISOString parity in the name
    fn iso_round_trip_matches_JS_toISOString() {
        // 2026-08-24T05:00:00.123Z and the epoch itself.
        for ms in [0i64, 1_787_547_600_123, 951_782_400_000, -86_400_000] {
            assert_eq!(parse_iso_ms(&iso_from_ms(ms)), Some(ms), "round trip {ms}");
        }
        assert_eq!(iso_from_ms(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso_from_ms(1_787_547_600_123), "2026-08-24T05:00:00.123Z");
    }
}
