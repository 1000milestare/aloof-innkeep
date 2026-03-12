use crate::{ics, google, models::Config};
use crate::models::CalendarEvent;

fn fmt_range(event: &CalendarEvent) -> String {
    let pdt = chrono::FixedOffset::west_opt(7 * 3600).unwrap();
    format!(
        "{} → {}",
        event.checkin.with_timezone(&pdt).format("%Y-%m-%d %H:%M %Z"),
        event.checkout.with_timezone(&pdt).format("%Y-%m-%d %H:%M %Z"),
    )
}

pub async fn sync_command(config: &Config, dry_run: bool) -> Result<(), Box<dyn std::error::Error>> {
    tracing::info!("Starting ICS sync");
    if dry_run { tracing::info!("DRY RUN — no events will be created"); }

    let ics_content = ics::fetch_ics(&config.ics_url).await?;
    let reservations = ics::parse_reservations(&ics_content)?;
    let events = ics::transform_to_calendar_events(
        reservations,
        &config.checkin_time,
        &config.checkout_time,
    )?;

    if dry_run {
        for (i, event) in events.iter().enumerate() {
            tracing::info!("[{}/{}] {} {}", i + 1, events.len(), event.reservation_id, fmt_range(event));
        }
        return Ok(());
    }

    let google_client = google::GoogleCalendarClient::new(
        config.google_calendar_id.clone(),
        &config.google_service_account_json,
    ).await?;

    for (i, event) in events.iter().enumerate() {
        let label = format!("[{}/{}] {}", i + 1, events.len(), event.reservation_id);
        match google_client.create_event(event).await {
            Ok(_)  => tracing::info!("{}: ✓ {}", label, fmt_range(event)),
            Err(e) => tracing::error!("{}: ✗ {}", label, e),
        }
    }

    tracing::info!("✓ Sync complete — {} events processed", events.len());
    Ok(())
}
