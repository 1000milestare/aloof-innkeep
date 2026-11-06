use crate::buffer::cleaning_buffer_for;
use crate::google::BookingSync;
use crate::models::CalendarEvent;
use crate::tz::{format_local, local_to_utc, LOCAL_TZ};
use crate::{google, ics, mailbox, models::Config};
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

/// Calendar events written at once. Each event is independent; the limit keeps the
/// Google API calls well inside its per-user quota.
const CONCURRENT_EVENTS: usize = 4;

fn fmt_range(event: &CalendarEvent) -> String {
    format!(
        "{} → {}",
        format_local(&event.checkin, "%Y-%m-%d %H:%M %Z"),
        format_local(&event.checkout, "%Y-%m-%d %H:%M %Z"),
    )
}

/// Add a `Guests:` line to an event description. Idempotent, so re-running with
/// the same count (or a description that already has one) never duplicates it.
fn with_guest_count(description: &str, guests: &str) -> String {
    if description.lines().any(|l| l.starts_with("Guests:")) {
        return description.to_string();
    }
    if description.is_empty() {
        format!("Guests: {}", guests)
    } else {
        format!("{}\nGuests: {}", description, guests)
    }
}

pub async fn sync_command(
    config: &Config,
    dry_run: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    tracing::info!("Starting ICS sync");
    if dry_run {
        tracing::info!("DRY RUN — no events will be created");
    }

    let ics_content = ics::fetch_ics(&config.ics_url).await?;
    let reservations = ics::parse_reservations(&ics_content)?;

    // First pass: build events with default times
    let mut events = ics::transform_to_calendar_events(
        reservations,
        &config.checkin_time,
        &config.checkout_time,
    )?;

    // Per-reservation check-in/checkout times from reminder emails and owner
    // commands. Mail comes from Cloudflare KV and/or Gmail, whichever is configured.
    let res_ids: Vec<String> = events.iter().map(|e| e.reservation_id.clone()).collect();

    // Reservations whose times came from a reminder or command this run. Only these
    // may update existing calendar events; default times never overwrite one.
    let mut reminder_times: HashSet<String> = HashSet::new();

    // Stored mail is read once and shared by the times lookup and the cancellation check.
    let kv_mail = mailbox::load_kv(config).await;

    let mails = match &kv_mail {
        Ok(kv) => mailbox::collect_with(config, &res_ids, kv).await,
        Err(e) => Err(anyhow::anyhow!("{}", e)),
    };
    match mails {
        Ok(mails) => {
            let times_map = mailbox::reservation_times(
                &mails,
                &res_ids,
                &config.command_sender,
                &config.checkin_time,
                &config.checkout_time,
            );
            {
                for event in &mut events {
                    if let Some(times) = times_map.get(&event.reservation_id) {
                        if let Some(guests) = &times.guest_count {
                            event.description = with_guest_count(&event.description, guests);
                        }

                        // Re-derive checkin/checkout with the email-specific times
                        let checkin_date = event.checkin.with_timezone(&LOCAL_TZ).date_naive();
                        let checkout_date = event.checkout.with_timezone(&LOCAL_TZ).date_naive();

                        if let (Ok(ci_time), Ok(co_time)) = (
                            chrono::NaiveTime::parse_from_str(&times.checkin_time, "%H:%M"),
                            chrono::NaiveTime::parse_from_str(&times.checkout_time, "%H:%M"),
                        ) {
                            let new_checkin =
                                local_to_utc(chrono::NaiveDateTime::new(checkin_date, ci_time));

                            let new_checkout =
                                local_to_utc(chrono::NaiveDateTime::new(checkout_date, co_time));

                            if let (Some(ci), Some(co)) = (new_checkin, new_checkout) {
                                if ci != event.checkin || co != event.checkout {
                                    tracing::info!(
                                        "Gmail override for {}: {} → {}",
                                        event.reservation_id,
                                        times.checkin_time,
                                        times.checkout_time
                                    );
                                }
                                event.checkin = ci;
                                event.checkout = co;
                                reminder_times.insert(event.reservation_id.clone());
                            }
                        }
                    }
                }
            }
        }
        Err(e) => {
            tracing::warn!("Mail lookup failed, using default times: {}", e);
        }
    }

    // Cancellation emails are authoritative, so never (re)create a booking they
    // name, even if the ICS feed still lists it. Otherwise each run would recreate
    // the event and buffer that the cancel step then deletes.
    let cancelled = match &kv_mail {
        Ok(kv) => mailbox::cancelled_ids_with(config, kv).await,
        Err(e) => Err(anyhow::anyhow!("{}", e)),
    };
    match cancelled {
        Ok(cancelled) => events.retain(|e| {
            let keep = !cancelled.contains(&e.reservation_id);
            if !keep {
                tracing::info!("{}: cancelled by email, not syncing", e.reservation_id);
            }
            keep
        }),
        Err(e) => crate::fail!("✗ Could not read cancellation emails: {}", e),
    }

    for conflict in crate::buffer::turnover_conflicts(&events, config.cleaning_buffer_hours) {
        tracing::warn!("Turnover conflict: {}", conflict);
    }

    if dry_run {
        for (i, event) in events.iter().enumerate() {
            tracing::info!(
                "[{}/{}] {} {}",
                i + 1,
                events.len(),
                event.reservation_id,
                fmt_range(event)
            );
        }
        return Ok(());
    }

    let google_client = google::GoogleCalendarClient::new(
        config.google_calendar_id.clone(),
        &config.google_service_account_json,
    )
    .await?;

    // Events are independent, so write them a few at a time. Each event still does its
    // calendar write before its cleaning buffer. A failure is counted, not fatal.
    let total = events.len();
    let google_client = Arc::new(google_client);
    let permits = Arc::new(Semaphore::new(CONCURRENT_EVENTS));
    let mut tasks = JoinSet::new();

    for (i, event) in events.into_iter().enumerate() {
        let google_client = Arc::clone(&google_client);
        let permits = Arc::clone(&permits);
        let authoritative = reminder_times.contains(&event.reservation_id);
        let hours = config.cleaning_buffer_hours;

        tasks.spawn(async move {
            let _permit = permits.acquire_owned().await;
            let label = format!("[{}/{}] {}", i + 1, total, event.reservation_id);

            match google_client
                .sync_booking_event(&event, authoritative)
                .await
            {
                Ok(BookingSync::Created) => {
                    tracing::info!("{}: ✓ created {}", label, fmt_range(&event))
                }
                Ok(BookingSync::Updated) => {
                    tracing::info!("{}: ✓ updated {}", label, fmt_range(&event))
                }
                Ok(BookingSync::Unchanged) => tracing::debug!("{}: unchanged", label),
                Err(e) => crate::fail!("{}: ✗ {}", label, e),
            }

            if let (Some(airbnb_id), Some(buffer)) = (
                event.booking_id.as_deref(),
                cleaning_buffer_for(&event, hours),
            ) {
                match google_client
                    .upsert_cleaning_buffer(airbnb_id, &buffer)
                    .await
                {
                    Ok(_) => tracing::info!(
                        "{}: ✓ cleaning buffer until {}",
                        label,
                        format_local(&buffer.checkout, "%Y-%m-%d %H:%M %Z")
                    ),
                    Err(e) => crate::fail!("{}: ✗ cleaning buffer: {}", label, e),
                }
            }
        });
    }

    while let Some(joined) = tasks.join_next().await {
        if let Err(e) = joined {
            crate::fail!("✗ Event task failed: {}", e);
        }
    }

    tracing::info!("✓ Sync complete — {} events processed", total);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guest_count_is_appended_once() {
        let d = with_guest_count("Reservation: ABC", "5 adults");
        assert_eq!(d, "Reservation: ABC\nGuests: 5 adults");
        assert_eq!(with_guest_count(&d, "5 adults"), d);
    }

    #[test]
    fn guest_count_on_empty_description() {
        assert_eq!(with_guest_count("", "2 adults"), "Guests: 2 adults");
    }

    #[test]
    fn guest_line_does_not_disturb_reservation_id_or_phone_extraction() {
        let desc = with_guest_count(
            "Reservation URL: https://www.airbnb.com/hosting/reservations/details/HMABC12345\nPhone Number (Last 4 Digits): 2250",
            "5 adults",
        );
        assert_eq!(
            crate::event_filter::extract_airbnb_id(&desc).as_deref(),
            Some("HMABC12345")
        );
        assert_eq!(
            crate::event_filter::extract_phone(&desc, "Reserved").as_deref(),
            Some("2250")
        );
    }
}
