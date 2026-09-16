//! Identifier and timestamp helpers: ids are
//! `<prefix>_<32 hex>` and timestamps are JavaScript `toISOString()` (millisecond `Z`).

use rand::RngCore;
use time::format_description::FormatItem;
use time::macros::format_description;
use time::OffsetDateTime;

const ISO_MS: &[FormatItem<'static>] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z");

pub fn new_id(prefix: &str) -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    let hex = hex::encode(bytes);
    if prefix.is_empty() {
        hex
    } else {
        format!("{prefix}_{hex}")
    }
}

pub fn iso_ms(t: OffsetDateTime) -> String {
    t.to_offset(time::UtcOffset::UTC)
        .format(ISO_MS)
        .expect("fixed ISO format never fails")
}

pub fn now_iso() -> String {
    iso_ms(OffsetDateTime::now_utc())
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn iso_matches_javascript_to_iso_string() {
        assert_eq!(iso_ms(datetime!(2026-09-15 03:04:05.006 UTC)), "2026-09-15T03:04:05.006Z");
        assert_eq!(iso_ms(datetime!(2026-01-01 00:00:00 UTC)), "2026-01-01T00:00:00.000Z");
    }

    #[test]
    fn ids_have_prefix_and_32_hex() {
        let id = new_id("sess");
        assert!(id.starts_with("sess_"));
        assert_eq!(id.len(), 5 + 32);
        assert_eq!(new_id("").len(), 32);
    }
}
