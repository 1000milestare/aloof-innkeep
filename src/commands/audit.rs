use crate::{google, ics, models::Config};
use std::collections::HashMap;

fn fmt_dt(dt: &chrono::DateTime<chrono::Utc>) -> String {
    let pdt = chrono::FixedOffset::west_opt(7 * 3600).unwrap();
    dt.with_timezone(&pdt).format("%Y-%m-%d %H:%M").to_string()
}

pub async fn audit_command(config: &Config) -> Result<(), Box<dyn std::error::Error>> {
    tracing::info!("Starting audit — read-only calendar sync check");

    // 1. Fetch and parse ICS (source of truth)
    let ics_content = ics::fetch_ics(&config.ics_url).await?;
    let reservations = ics::parse_reservations(&ics_content)?;
    let expected = ics::transform_to_calendar_events(
        reservations,
        &config.checkin_time,
        &config.checkout_time,
    )?;

    tracing::info!("ICS: {} reservations", expected.len());

    // 2. Fetch Google Calendar events
    let google_client = google::GoogleCalendarClient::new(
        config.google_calendar_id.clone(),
        &config.google_service_account_json,
    )
    .await?;
    let gcal_events = google_client.list_bookable_events().await?;

    tracing::info!("Google Calendar: {} bookable events", gcal_events.len());

    // 3. Index Google Calendar events by booking_id (Airbnb reservation ID)
    let mut gcal_by_id: HashMap<String, _> = HashMap::new();
    for event in &gcal_events {
        if let Some(ref bid) = event.booking_id {
            gcal_by_id.insert(bid.clone(), event);
        }
    }

    let mut issues = 0;

    // 4. Check each ICS reservation against Google Calendar
    for ics_event in &expected {
        let rid = &ics_event.reservation_id;
        match gcal_by_id.remove(rid) {
            None => {
                tracing::warn!(
                    "MISSING in Google Calendar: {} ({} → {})",
                    rid,
                    fmt_dt(&ics_event.checkin),
                    fmt_dt(&ics_event.checkout)
                );
                issues += 1;
            }
            Some(gcal_event) => {
                // Compare check-in times (allow 1-minute tolerance for rounding)
                let checkin_diff = (ics_event.checkin - gcal_event.checkin).num_minutes().abs();
                let checkout_diff = (ics_event.checkout - gcal_event.checkout)
                    .num_minutes()
                    .abs();

                if checkin_diff > 1 || checkout_diff > 1 {
                    tracing::warn!(
                        "TIME MISMATCH: {} | ICS: {} → {} | GCal: {} → {}",
                        rid,
                        fmt_dt(&ics_event.checkin),
                        fmt_dt(&ics_event.checkout),
                        fmt_dt(&gcal_event.checkin),
                        fmt_dt(&gcal_event.checkout),
                    );
                    issues += 1;
                } else {
                    tracing::info!(
                        "OK: {} ({} → {})",
                        rid,
                        fmt_dt(&ics_event.checkin),
                        fmt_dt(&ics_event.checkout)
                    );
                }
            }
        }
    }

    // 5. Any remaining in gcal_by_id are extra (in Google but not in ICS)
    for (rid, event) in &gcal_by_id {
        tracing::warn!(
            "EXTRA in Google Calendar (not in ICS): {} \"{}\" ({} → {})",
            rid,
            event.title,
            fmt_dt(&event.checkin),
            fmt_dt(&event.checkout),
        );
        issues += 1;
    }

    // 6. Summary
    if issues == 0 {
        tracing::info!(
            "✓ Audit passed — all {} reservations in sync",
            expected.len()
        );
    } else {
        tracing::warn!(
            "✗ Audit found {} issue(s) across {} reservations",
            issues,
            expected.len()
        );
    }

    Ok(())
}
