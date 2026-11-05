//! Remove calendar events and lock codes for reservations that have dropped out
//! of the ICS feed (cancellations).
//!
//! Safety:
//! - only calendar events carrying an Airbnb reservation ID are candidates
//! - only the lock code named after that same reservation is deleted; other
//!   codes on the device (owner, cleaner) are never touched
//! - the code is deleted first; if that fails the calendar event is kept so the
//!   next run retries
//! - `plan_cancellations` refuses to act on an empty feed or a large batch

use crate::reconcile::plan_cancellations;
use crate::tz::format_local;
use crate::{google, ics, models::Config, seam};

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

    let cancelled = match plan_cancellations(&expected, &gcal_events) {
        Ok(c) => c,
        Err(guard) => {
            // Not fatal: later steps in a full run should still go ahead.
            tracing::error!("✗ Skipping cancellations: {}", guard);
            return Ok(());
        }
    };

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
                                tracing::error!("{} - ✗ Could not delete lock code: {}", name, e);
                                code_ok = false;
                            }
                        }
                    }
                }
                Ok(None) => tracing::info!("{} - No lock code on device", name),
                Err(e) => {
                    tracing::error!("{} - ✗ Could not read lock codes: {}", name, e);
                    code_ok = false;
                }
            }
        }

        if !code_ok {
            tracing::warn!("{} - Keeping calendar event so the next run retries", name);
            continue;
        }

        if dry_run {
            tracing::info!("{} - Would delete calendar event ({})", name, when);
            continue;
        }
        match google_client.delete_event(&event.reservation_id).await {
            Ok(()) => tracing::info!("{} - ✓ Deleted calendar event ({})", name, when),
            Err(e) => tracing::error!("{} - ✗ Could not delete calendar event: {}", name, e),
        }
    }

    Ok(())
}
