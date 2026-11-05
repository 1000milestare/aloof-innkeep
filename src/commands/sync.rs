use crate::models::CalendarEvent;
use crate::tz::{format_local, local_to_utc, LOCAL_TZ};
use crate::{gmail, google, ics, models::Config};

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

    // If Gmail is configured, look up per-reservation check-in/checkout times
    let gmail_configured = !config.gmail_client_id.is_empty()
        && !config.gmail_client_secret.is_empty()
        && !config.gmail_refresh_token.is_empty();

    if gmail_configured {
        tracing::info!("Gmail configured — looking up per-reservation times");
        let res_ids: Vec<String> = events.iter().map(|e| e.reservation_id.clone()).collect();

        match gmail::fetch_reservation_times(
            &config.gmail_client_id,
            &config.gmail_client_secret,
            &config.gmail_refresh_token,
            &res_ids,
        )
        .await
        {
            Ok(times_map) => {
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
                            }
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!("Gmail lookup failed, using default times: {}", e);
            }
        }
    } else {
        tracing::debug!("Gmail not configured — using default check-in/checkout times");
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

    for (i, event) in events.iter().enumerate() {
        let label = format!("[{}/{}] {}", i + 1, events.len(), event.reservation_id);
        match google_client.create_event(event).await {
            Ok(_) => tracing::info!("{}: ✓ {}", label, fmt_range(event)),
            Err(e) => tracing::error!("{}: ✗ {}", label, e),
        }
    }

    tracing::info!("✓ Sync complete — {} events processed", events.len());
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
