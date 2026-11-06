//! Remove calendar events and lock codes for cancelled reservations.
//!
//! A reservation counts as cancelled when either:
//! - Airbnb cancellation emails name it (the source of truth), or
//! - it has dropped out of the ICS feed (`plan_cancellations`).
//!
//! Safety:
//! - only calendar events carrying an Airbnb reservation ID are candidates, and
//!   only stays starting today or later (list_bookable_events enforces this)
//! - only the lock code named after that same reservation is deleted; other
//!   codes on the device (owner, cleaner) are never touched
//! - the code is deleted first; if that fails the calendar event is kept so the
//!   next run retries
//! - the feed path is refused on an empty feed or a large batch; email-confirmed
//!   cancellations are not subject to that cap

use crate::models::CalendarEvent;
use crate::reconcile::plan_cancellations;
use crate::tz::format_local;
use crate::{google, ics, mailbox, models::Config, seam};
use std::collections::HashSet;

/// How far back cancellation emails are searched.
pub const CANCELLATION_LOOKBACK_DAYS: u32 = 90;

pub async fn cancel_command(
    config: &Config,
    dry_run: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    tracing::info!("Checking for cancelled reservations");
    if dry_run {
        tracing::info!("DRY RUN MODE - nothing will be deleted");
    }

    let ics_content = ics::fetch_ics(&config.ics_url).await?;
    let reservations = ics::parse_reservations(&ics_content)?;
    let expected = ics::transform_to_calendar_events(
        reservations,
        &config.checkin_time,
        &config.checkout_time,
    )?;

    let google_client = google::GoogleCalendarClient::new(
        config.google_calendar_id.clone(),
        &config.google_service_account_json,
    )
    .await?;
    let gcal_events = google_client.list_bookable_events().await?;

    // Feed-based cancellations. A refused guard (empty feed, too many) only stops
    // this path; email-confirmed cancellations below are still processed.
    let mut cancelled: Vec<&CalendarEvent> = match plan_cancellations(&expected, &gcal_events) {
        Ok(c) => c,
        Err(guard) => {
            crate::fail!("✗ Feed-based cancellations skipped: {}", guard);
            Vec::new()
        }
    };
    let mut seen: HashSet<String> = cancelled
        .iter()
        .filter_map(|e| e.booking_id.clone())
        .collect();

    // Cancellation emails are the source of truth (there is no Airbnb API). Only
    // calendar events are candidates, and list_bookable_events has already dropped
    // stays that started before today, so this only acts on today-or-later stays.
    match mailbox::cancelled_ids(config).await {
        Ok(codes) => {
            for event in &gcal_events {
                if let Some(bid) = event.booking_id.as_deref() {
                    if codes.contains(bid) && seen.insert(bid.to_string()) {
                        tracing::info!("{} - cancellation confirmed by email", bid);
                        cancelled.push(event);
                    }
                }
            }
        }
        Err(e) => crate::fail!("✗ Could not read cancellation emails: {}", e),
    }

    if cancelled.is_empty() {
        tracing::info!("✓ No cancellations");
        return Ok(());
    }

    let seam_client = if config.seam_api_key.is_empty() || config.seam_device_id.is_empty() {
        tracing::warn!("Seam not configured - only calendar events will be removed");
        None
    } else {
        Some(seam::SeamClient::new(
            config.seam_api_key.clone(),
            config.seam_device_id.clone(),
        ))
    };

    for event in cancelled {
        let name = event.booking_id.as_deref().unwrap_or(&event.reservation_id);
        let when = format!(
            "{} → {}",
            format_local(&event.checkin, "%Y-%m-%d %H:%M"),
            format_local(&event.checkout, "%Y-%m-%d %H:%M"),
        );

        let mut code_ok = true;
        if let Some(seam_client) = &seam_client {
            match seam_client.find_existing_code(event).await {
                Ok(Some(code)) => {
                    if dry_run {
                        tracing::info!("{} - Would delete lock code ({})", name, when);
                    } else {
                        match seam_client.delete_access_code(&code.access_code_id).await {
                            Ok(()) => tracing::info!("{} - ✓ Deleted lock code", name),
                            Err(e) => {
                                crate::fail!("{} - ✗ Could not delete lock code: {}", name, e);
                                code_ok = false;
                            }
                        }
                    }
                }
                Ok(None) => tracing::info!("{} - No lock code on device", name),
                Err(e) => {
                    crate::fail!("{} - ✗ Could not read lock codes: {}", name, e);
                    code_ok = false;
                }
            }
        }

        if !code_ok {
            tracing::warn!("{} - Keeping calendar event so the next run retries", name);
            continue;
        }

        if let Some(airbnb_id) = event.booking_id.as_deref() {
            // Buffer goes first: if it can't be removed we keep the booking,
            // so the next run still sees the cancellation and retries.
            if dry_run {
                tracing::info!("{} - Would delete cleaning buffer", name);
            } else if let Err(e) = google_client.delete_cleaning_buffer(airbnb_id).await {
                crate::fail!("{} - ✗ Could not delete cleaning buffer: {}", name, e);
                tracing::warn!("{} - Keeping calendar event so the next run retries", name);
                continue;
            } else {
                tracing::info!("{} - ✓ Deleted cleaning buffer", name);
            }
        }

        if dry_run {
            tracing::info!("{} - Would delete calendar event ({})", name, when);
            continue;
        }
        match google_client.delete_event(&event.reservation_id).await {
            Ok(()) => tracing::info!("{} - ✓ Deleted calendar event ({})", name, when),
            Err(e) => crate::fail!("{} - ✗ Could not delete calendar event: {}", name, e),
        }
    }

    Ok(())
}
