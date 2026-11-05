//! Время по protocol.md §1.1: `ts` конвертов — ISO-8601 UTC, игровая истина
//! времени — epoch-секунды в payload'ах.

use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

/// ISO-8601 UTC для поля `ts` (§2.1).
pub fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .expect("формат RFC3339")
}

/// Epoch-секунды — игровое время (`server_ts`, `expires_at`, §1.1).
pub fn now_epoch() -> i64 {
    OffsetDateTime::now_utc().unix_timestamp()
}
