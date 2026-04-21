use crate::models::CalendarEvent;
use crate::{gmail, google, ics, models::Config};
use chrono::TimeZone;

fn fmt_range(event: &CalendarEvent) -> String {
    let pdt = chrono::FixedOffset::west_opt(7 * 3600).unwrap();
    format!(
        "{} → {}",
        event
            .checkin
            .with_timezone(&pdt)
            .format("%Y-%m-%d %H:%M %Z"),
        event
            .checkout
            .with_timezone(&pdt)
            .format("%Y-%m-%d %H:%M %Z"),
    )
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
                        // Re-derive checkin/checkout with the email-specific times
                        let pdt = chrono::FixedOffset::west_opt(7 * 3600).unwrap();
                        let checkin_date = event.checkin.with_timezone(&pdt).date_naive();
                        let checkout_date = event.checkout.with_timezone(&pdt).date_naive();

                        if let (Ok(ci_time), Ok(co_time)) = (
                            chrono::NaiveTime::parse_from_str(&times.checkin_time, "%H:%M"),
                            chrono::NaiveTime::parse_from_str(&times.checkout_time, "%H:%M"),
                        ) {
                            let new_checkin = pdt
                                .from_local_datetime(&chrono::NaiveDateTime::new(
                                    checkin_date,
                                    ci_time,
                                ))
                                .single()
                                .map(|dt| dt.with_timezone(&chrono::Utc));

                            let new_checkout = pdt
                                .from_local_datetime(&chrono::NaiveDateTime::new(
                                    checkout_date,
                                    co_time,
                                ))
                                .single()
                                .map(|dt| dt.with_timezone(&chrono::Utc));

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
