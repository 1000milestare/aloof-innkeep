//! Post-checkout cleaning buffer.
//!
//! For each booking, a separate calendar block runs from checkout for
//! `CLEANING_BUFFER_HOURS` (default 3). Platforms that read the calendar
//! (e.g. Peerspace) then see the slot as blocked, so nothing can be booked
//! before cleaning is possible.
//!
//! The buffer is deliberately NOT a booking:
//! - its `booking_id` is `None`, so cancellation and reconciliation ignore it
//! - its title and description carry no "reserved" marker and no phone
//!   number, so `list_bookable_events` skips it and no lock code is created
//!   for it
//! - it is never used as the guest's access window; the booking event keeps
//!   the real check-in/checkout times

use crate::models::CalendarEvent;
use chrono::Duration;

/// iCalUID for a booking's cleaning buffer. Distinct from the booking's own
/// `<id>@airbnb-sync.local` so the two can be looked up independently.
pub fn buffer_ical_uid(airbnb_id: &str) -> String {
    format!("{}-buffer@airbnb-sync.local", airbnb_id)
}

/// Build the cleaning-buffer event for a booking: from checkout for `hours`.
/// Returns `None` when the buffer is disabled (`hours == 0`) or the booking
/// has no Airbnb ID.
pub fn cleaning_buffer_for(booking: &CalendarEvent, hours: u32) -> Option<CalendarEvent> {
    if hours == 0 {
        return None;
    }
    let airbnb_id = booking.booking_id.as_deref()?;
    Some(CalendarEvent {
        title: format!("Cleaning buffer ({})", airbnb_id),
        description: "Post-checkout cleaning buffer. Not a guest booking.".to_string(),
        checkin: booking.checkout,
        checkout: booking.checkout + Duration::hours(i64::from(hours)),
        reservation_id: buffer_ical_uid(airbnb_id),
        booking_id: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_filter::extract_phone;
    use chrono::{TimeZone, Utc};

    fn booking() -> CalendarEvent {
        CalendarEvent {
            title: "Reserved: HMABC12345".to_string(),
            description: "Reservation: HMABC12345".to_string(),
            checkin: Utc.with_ymd_and_hms(2026, 11, 2, 23, 0, 0).unwrap(),
            checkout: Utc.with_ymd_and_hms(2026, 11, 5, 18, 0, 0).unwrap(),
            reservation_id: "HMABC12345".to_string(),
            booking_id: Some("HMABC12345".to_string()),
        }
    }

    #[test]
    fn buffer_starts_at_checkout_and_lasts_three_hours_by_default() {
        let b = booking();
        let buf = cleaning_buffer_for(&b, 3).unwrap();
        assert_eq!(buf.checkin, b.checkout);
        assert_eq!(buf.checkout, b.checkout + Duration::hours(3));
    }

    #[test]
    fn zero_hours_disables_the_buffer() {
        assert!(cleaning_buffer_for(&booking(), 0).is_none());
    }

    #[test]
    fn buffer_is_not_a_booking() {
        let buf = cleaning_buffer_for(&booking(), 3).unwrap();
        assert!(buf.booking_id.is_none());
        assert!(!buf.title.to_lowercase().contains("reserved"));
        assert!(extract_phone(&buf.description, &buf.title).is_none());
    }

    #[test]
    fn buffer_uid_differs_from_booking_uid() {
        assert_eq!(
            buffer_ical_uid("HMABC12345"),
            "HMABC12345-buffer@airbnb-sync.local"
        );
    }
}
