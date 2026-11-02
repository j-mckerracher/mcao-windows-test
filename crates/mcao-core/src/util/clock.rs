//! The millisecond clock the shell scripts read as `int(time.time() * 1000)`.

use std::time::{SystemTime, UNIX_EPOCH};

/// `int(secs * 1000)` for a float epoch time in seconds. Truncates toward zero, as `int`
/// does, so a float that lands just under a whole millisecond reads one millisecond low.
pub fn ms_from_secs(secs: f64) -> i64 {
    (secs * 1000.0) as i64
}

/// Current epoch milliseconds, formed the way the Python reference forms them: the epoch
/// time as a float in seconds, then `ms_from_secs`.
pub fn now_ms() -> i64 {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    ms_from_secs(elapsed.as_nanos() as f64 / 1e9)
}
