//! Shared local-timezone handling.
//!
//! All properties operate out of Crockett, California, so every
//! check-in/check-out wall-clock time in this codebase is interpreted in
//! `America/Los_Angeles`. Using the IANA zone (rather than a fixed UTC
//! offset) means conversions automatically follow the correct DST rules
//! instead of assuming PDT (UTC-7) year-round, which is wrong for roughly
//! half the year while standard time (PST, UTC-8) is in effect.

use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;

pub const LOCAL_TZ: Tz = chrono_tz::America::Los_Angeles;

/// Interpret a naive local date/time (wall-clock, Pacific) as a UTC instant,
/// correctly accounting for PST/PDT depending on the date.
///
/// Returns `None` for a local time that doesn't exist (the "spring forward"
/// gap) or is ambiguous (the "fall back" overlap) — callers should surface
/// that as an error rather than silently guessing.
pub fn local_to_utc(naive: NaiveDateTime) -> Option<DateTime<Utc>> {
    LOCAL_TZ
        .from_local_datetime(&naive)
        .single()
        .map(|dt| dt.with_timezone(&Utc))
}

/// Format a UTC instant as local Pacific wall-clock time, e.g. for logging.
pub fn format_local(dt: &DateTime<Utc>, fmt: &str) -> String {
    dt.with_timezone(&LOCAL_TZ).format(fmt).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn ymd_hm(y: i32, m: u32, d: u32, h: u32, min: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(h, min, 0)
            .unwrap()
    }

    #[test]
    fn standard_time_uses_utc_minus_8() {
        // Nov 2, 2026 is after the 2026 "fall back" (Nov 1), so 15:00
        // local should be PST (UTC-8), i.e. 23:00 UTC.
        let utc = local_to_utc(ymd_hm(2026, 11, 2, 15, 0)).unwrap();
        assert_eq!(utc, Utc.with_ymd_and_hms(2026, 11, 2, 23, 0, 0).unwrap());
    }

    #[test]
    fn daylight_time_uses_utc_minus_7() {
        // July is deep in PDT (UTC-7): 15:00 local -> 22:00 UTC.
        let utc = local_to_utc(ymd_hm(2026, 7, 15, 15, 0)).unwrap();
        assert_eq!(utc, Utc.with_ymd_and_hms(2026, 7, 15, 22, 0, 0).unwrap());
    }

    #[test]
    fn checkin_shortly_before_fall_back_is_still_pdt() {
        // Oct 31, 2026 (before the Nov 1 fall-back) is still PDT (UTC-7).
        let utc = local_to_utc(ymd_hm(2026, 10, 31, 15, 0)).unwrap();
        assert_eq!(utc, Utc.with_ymd_and_hms(2026, 10, 31, 22, 0, 0).unwrap());
    }

    #[test]
    fn checkin_shortly_after_fall_back_is_pst() {
        // Nov 2, 2026 (after the Nov 1 fall-back) is PST (UTC-8). A naive
        // fixed -7 offset would make this look an hour early, which is
        // exactly the bug this module fixes.
        let utc = local_to_utc(ymd_hm(2026, 11, 2, 15, 0)).unwrap();
        assert_eq!(utc, Utc.with_ymd_and_hms(2026, 11, 2, 23, 0, 0).unwrap());
    }

    #[test]
    fn spring_forward_gap_returns_none() {
        // 2026's "spring forward" is Mar 8, 2:00 AM -> 3:00 AM; 2:30 AM
        // never happens locally.
        assert!(local_to_utc(ymd_hm(2026, 3, 8, 2, 30)).is_none());
    }

    #[test]
    fn fall_back_ambiguous_time_returns_none() {
        // 2026's "fall back" is Nov 1, 2:00 AM; 1:30 AM happens twice.
        assert!(local_to_utc(ymd_hm(2026, 11, 1, 1, 30)).is_none());
    }

    #[test]
    fn format_local_reflects_dst_label() {
        let winter = Utc.with_ymd_and_hms(2026, 11, 2, 23, 0, 0).unwrap();
        assert_eq!(
            format_local(&winter, "%Y-%m-%d %H:%M %Z"),
            "2026-11-02 15:00 PST"
        );

        let summer = Utc.with_ymd_and_hms(2026, 7, 15, 22, 0, 0).unwrap();
        assert_eq!(
            format_local(&summer, "%Y-%m-%d %H:%M %Z"),
            "2026-07-15 15:00 PDT"
        );
    }
}
