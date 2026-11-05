//! Shared logic for comparing the ICS feed's expected check-in/checkout
//! times against what's actually on Google Calendar.
//!
//! Used by both `audit` (read-only reporting) and `fix-times` (the one-off
//! correction script), so the two can never disagree about what counts as
//! a discrepancy.

use crate::models::CalendarEvent;
use std::collections::HashMap;

/// How far apart two timestamps can be before they're still considered
/// "the same", to absorb rounding/serialization jitter.
pub const TOLERANCE_MINUTES: i64 = 1;

/// An ICS-expected event whose Google Calendar counterpart has different
/// checkin/checkout times.
pub struct Mismatch<'a> {
    /// The Airbnb reservation ID (shared key between ICS and GCal).
    pub reservation_id: &'a str,
    /// What the ICS feed says it should be (source of truth).
    pub expected: &'a CalendarEvent,
    /// What's actually on Google Calendar. `actual.reservation_id` here is
    /// the Google Calendar event ID (see `list_bookable_events`), needed to
    /// address the event for an update.
    pub actual: &'a CalendarEvent,
}

pub struct ReconcileReport<'a> {
    /// In the ICS feed but missing from Google Calendar entirely.
    pub missing: Vec<&'a CalendarEvent>,
    /// Present on both sides but with different checkin/checkout times.
    pub mismatched: Vec<Mismatch<'a>>,
    /// On Google Calendar but not in the ICS feed.
    pub extra: Vec<&'a CalendarEvent>,
    /// Present on both sides and already in agreement.
    pub ok: Vec<&'a CalendarEvent>,
}

impl ReconcileReport<'_> {
    pub fn issue_count(&self) -> usize {
        self.missing.len() + self.mismatched.len() + self.extra.len()
    }
}

/// Compare ICS-expected events against what's on Google Calendar, matched
/// by the Airbnb reservation ID (`expected.reservation_id` against
/// `gcal.booking_id`, which `list_bookable_events` extracts from the
/// event description).
pub fn reconcile<'a>(
    expected: &'a [CalendarEvent],
    gcal: &'a [CalendarEvent],
) -> ReconcileReport<'a> {
    let mut gcal_by_id: HashMap<&str, &CalendarEvent> = HashMap::new();
    for event in gcal {
        if let Some(ref bid) = event.booking_id {
            gcal_by_id.insert(bid.as_str(), event);
        }
    }

    let mut missing = Vec::new();
    let mut mismatched = Vec::new();
    let mut ok = Vec::new();

    for exp in expected {
        match gcal_by_id.remove(exp.reservation_id.as_str()) {
            None => missing.push(exp),
            Some(actual) => {
                let checkin_diff = (exp.checkin - actual.checkin).num_minutes().abs();
                let checkout_diff = (exp.checkout - actual.checkout).num_minutes().abs();

                if checkin_diff > TOLERANCE_MINUTES || checkout_diff > TOLERANCE_MINUTES {
                    mismatched.push(Mismatch {
                        reservation_id: exp.reservation_id.as_str(),
                        expected: exp,
                        actual,
                    });
                } else {
                    ok.push(exp);
                }
            }
        }
    }

    let extra = gcal_by_id.into_values().collect();

    ReconcileReport {
        missing,
        mismatched,
        extra,
        ok,
    }
}

/// Most cancellations one run is allowed to act on. A feed outage or a parse
/// regression looks exactly like "everything was cancelled", so anything above
/// this is refused and left for a human.
pub const MAX_CANCELLATIONS_PER_RUN: usize = 5;

#[derive(Debug, PartialEq, Eq)]
pub enum CancellationGuard {
    /// The ICS feed returned no reservations at all while the calendar has some.
    EmptyFeed { calendar_reservations: usize },
    /// More cancellations than `MAX_CANCELLATIONS_PER_RUN`.
    TooMany { found: usize },
}

impl std::fmt::Display for CancellationGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CancellationGuard::EmptyFeed {
                calendar_reservations,
            } => write!(
                f,
                "ICS feed has no reservations but the calendar has {}; assuming a feed problem",
                calendar_reservations
            ),
            CancellationGuard::TooMany { found } => write!(
                f,
                "{} cancellations found (limit {}); refusing to act automatically",
                found, MAX_CANCELLATIONS_PER_RUN
            ),
        }
    }
}

/// Calendar events for Airbnb reservations that are no longer in the ICS feed.
///
/// Only events carrying an Airbnb reservation ID (`booking_id`) are candidates,
/// so manually entered events (e.g. other platforms) are never treated as
/// cancellations. `gcal` is expected to hold future events only.
pub fn plan_cancellations<'a>(
    expected: &[CalendarEvent],
    gcal: &'a [CalendarEvent],
) -> Result<Vec<&'a CalendarEvent>, CancellationGuard> {
    let live: std::collections::HashSet<&str> =
        expected.iter().map(|e| e.reservation_id.as_str()).collect();

    let airbnb_events: Vec<&CalendarEvent> =
        gcal.iter().filter(|e| e.booking_id.is_some()).collect();

    let cancelled: Vec<&CalendarEvent> = airbnb_events
        .iter()
        .copied()
        .filter(|e| !live.contains(e.booking_id.as_deref().unwrap_or("")))
        .collect();

    if expected.is_empty() && !airbnb_events.is_empty() {
        return Err(CancellationGuard::EmptyFeed {
            calendar_reservations: airbnb_events.len(),
        });
    }
    if cancelled.len() > MAX_CANCELLATIONS_PER_RUN {
        return Err(CancellationGuard::TooMany {
            found: cancelled.len(),
        });
    }
    Ok(cancelled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn event(reservation_id: &str, checkin_hour: u32, checkout_hour: u32) -> CalendarEvent {
        CalendarEvent {
            title: format!("Reserved: {}", reservation_id),
            description: String::new(),
            checkin: Utc
                .with_ymd_and_hms(2026, 11, 2, checkin_hour, 0, 0)
                .unwrap(),
            checkout: Utc
                .with_ymd_and_hms(2026, 11, 5, checkout_hour, 0, 0)
                .unwrap(),
            reservation_id: reservation_id.to_string(),
            booking_id: Some(reservation_id.to_string()),
        }
    }

    /// Simulate a Google Calendar event: `reservation_id` is the GCal event
    /// id (distinct from the Airbnb id), `booking_id` is the Airbnb id
    /// extracted from the description, same as `list_bookable_events`.
    fn gcal_event(
        gcal_id: &str,
        airbnb_id: &str,
        checkin_hour: u32,
        checkout_hour: u32,
    ) -> CalendarEvent {
        CalendarEvent {
            title: format!("Reserved: {}", airbnb_id),
            description: String::new(),
            checkin: Utc
                .with_ymd_and_hms(2026, 11, 2, checkin_hour, 0, 0)
                .unwrap(),
            checkout: Utc
                .with_ymd_and_hms(2026, 11, 5, checkout_hour, 0, 0)
                .unwrap(),
            reservation_id: gcal_id.to_string(),
            booking_id: Some(airbnb_id.to_string()),
        }
    }

    #[test]
    fn matching_times_are_ok() {
        let expected = vec![event("ABC123", 23, 18)];
        let gcal = vec![gcal_event("gcal-1", "ABC123", 23, 18)];

        let report = reconcile(&expected, &gcal);
        assert_eq!(report.ok.len(), 1);
        assert!(report.mismatched.is_empty());
        assert!(report.missing.is_empty());
        assert!(report.extra.is_empty());
        assert_eq!(report.issue_count(), 0);
    }

    #[test]
    fn dst_offset_bug_shows_up_as_an_hour_mismatch() {
        // Simulates exactly the historical bug: GCal has the event an hour
        // early (created back when the sync used a fixed UTC-7 offset
        // during what should have been PST).
        let expected = vec![event("ABC123", 23, 18)];
        let gcal = vec![gcal_event("gcal-1", "ABC123", 22, 17)];

        let report = reconcile(&expected, &gcal);
        assert!(report.ok.is_empty());
        assert_eq!(report.mismatched.len(), 1);
        assert_eq!(report.mismatched[0].reservation_id, "ABC123");
        assert_eq!(report.mismatched[0].actual.reservation_id, "gcal-1");
        assert_eq!(report.issue_count(), 1);
    }

    #[test]
    fn missing_from_gcal_is_reported() {
        let expected = vec![event("ABC123", 23, 18)];
        let gcal: Vec<CalendarEvent> = vec![];

        let report = reconcile(&expected, &gcal);
        assert_eq!(report.missing.len(), 1);
        assert_eq!(report.issue_count(), 1);
    }

    #[test]
    fn extra_in_gcal_is_reported() {
        let expected: Vec<CalendarEvent> = vec![];
        let gcal = vec![gcal_event("gcal-1", "ZZZ999", 23, 18)];

        let report = reconcile(&expected, &gcal);
        assert_eq!(report.extra.len(), 1);
        assert_eq!(report.issue_count(), 1);
    }

    #[test]
    fn one_minute_jitter_is_tolerated() {
        let expected = vec![CalendarEvent {
            checkin: Utc.with_ymd_and_hms(2026, 11, 2, 23, 0, 30).unwrap(),
            ..event("ABC123", 23, 18)
        }];
        let gcal = vec![gcal_event("gcal-1", "ABC123", 23, 18)];

        let report = reconcile(&expected, &gcal);
        assert_eq!(report.ok.len(), 1);
        assert!(report.mismatched.is_empty());
    }

    #[test]
    fn cancellation_is_calendar_event_missing_from_feed() {
        let expected = vec![event("KEEP1", 23, 18)];
        let gcal = vec![
            gcal_event("g1", "KEEP1", 23, 18),
            gcal_event("g2", "GONE1", 23, 18),
        ];
        let cancelled = plan_cancellations(&expected, &gcal).unwrap();
        assert_eq!(cancelled.len(), 1);
        assert_eq!(cancelled[0].reservation_id, "g2");
    }

    #[test]
    fn manual_events_without_airbnb_id_are_never_cancelled() {
        let expected = vec![event("KEEP1", 23, 18)];
        let mut manual = gcal_event("g9", "X", 23, 18);
        manual.booking_id = None;
        let gcal = vec![gcal_event("g1", "KEEP1", 23, 18), manual];
        assert!(plan_cancellations(&expected, &gcal).unwrap().is_empty());
    }

    #[test]
    fn empty_feed_is_refused() {
        let gcal = vec![gcal_event("g1", "KEEP1", 23, 18)];
        assert_eq!(
            plan_cancellations(&[], &gcal).unwrap_err(),
            CancellationGuard::EmptyFeed {
                calendar_reservations: 1
            }
        );
    }

    #[test]
    fn empty_feed_and_empty_calendar_is_fine() {
        assert!(plan_cancellations(&[], &[]).unwrap().is_empty());
    }

    #[test]
    fn too_many_cancellations_is_refused() {
        let expected = vec![event("KEEP1", 23, 18)];
        let mut gcal = vec![gcal_event("g0", "KEEP1", 23, 18)];
        for i in 0..=MAX_CANCELLATIONS_PER_RUN {
            gcal.push(gcal_event(
                &format!("g{}", i + 1),
                &format!("GONE{}", i),
                23,
                18,
            ));
        }
        assert_eq!(
            plan_cancellations(&expected, &gcal).unwrap_err(),
            CancellationGuard::TooMany {
                found: MAX_CANCELLATIONS_PER_RUN + 1
            }
        );
    }

    #[test]
    fn exactly_the_limit_is_allowed() {
        let expected = vec![event("KEEP1", 23, 18)];
        let gcal: Vec<CalendarEvent> = (0..MAX_CANCELLATIONS_PER_RUN)
            .map(|i| gcal_event(&format!("g{}", i), &format!("GONE{}", i), 23, 18))
            .collect();
        assert_eq!(
            plan_cancellations(&expected, &gcal).unwrap().len(),
            MAX_CANCELLATIONS_PER_RUN
        );
    }
}
