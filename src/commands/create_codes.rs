use crate::models::CalendarEvent;
use crate::{event_filter, google, models::Config, seam};

/// Format checkin/checkout as "YYYY-MM-DD HH:MM PDT" for logging.
fn fmt_range(event: &CalendarEvent) -> String {
    let pdt = chrono::FixedOffset::west_opt(7 * 3600).unwrap();
    format!(
        "{} → {}",
        event.checkin.with_timezone(&pdt).format("%Y-%m-%d %H:%M"),
        event.checkout.with_timezone(&pdt).format("%Y-%m-%d %H:%M"),
    )
}

pub async fn create_codes_command(
    config: &Config,
    dry_run: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    tracing::info!("Creating smart lock codes from calendar");
    if dry_run {
        tracing::info!("DRY RUN MODE - No codes will be created");
    }

    if config.seam_api_key.is_empty() || config.seam_device_id.is_empty() {
        tracing::warn!("Seam API not configured - skipping code creation");
        return Ok(());
    }

    // Fetch events: already filtered to "reserved"/has-phone + today-or-later
    let google_client = google::GoogleCalendarClient::new(
        config.google_calendar_id.clone(),
        &config.google_service_account_json,
    )
    .await?;

    let events = google_client.list_bookable_events().await?;
    tracing::info!("Processing {} bookable future events", events.len());

    let seam_client =
        seam::SeamClient::new(config.seam_api_key.clone(), config.seam_device_id.clone());

    for (idx, event) in events.iter().enumerate() {
        let name = event.booking_id.as_deref().unwrap_or(&event.reservation_id);
        let label = format!("[Code {}/{}] {}", idx + 1, events.len(), name);

        tracing::debug!(
            "{} | gcal_id={} | checkin={} | phone={:?} | desc={:?}",
            label,
            event.reservation_id,
            event
                .checkin
                .with_timezone(&chrono::FixedOffset::west_opt(7 * 3600).unwrap())
                .format("%Y-%m-%d %H:%M"),
            event_filter::extract_phone(&event.description, &event.title),
            &event.description[..event.description.len().min(200)]
        );

        let phone = event_filter::extract_phone(&event.description, &event.title);

        let Some(code) = phone else {
            tracing::info!("{} - No phone found, skipping", label);
            continue;
        };

        if dry_run {
            tracing::info!(
                "{} - Would create code: {} ({})",
                label,
                code,
                fmt_range(event)
            );
            continue;
        }

        // Check if code already exists (by Airbnb ID or Google event ID)
        match seam_client.code_exists(event).await {
            Ok(true) => {
                tracing::info!("{} - Code already exists, skipping", label);
                continue;
            }
            Ok(false) => {}
            Err(e) => tracing::warn!("{} - Could not check existing codes: {}", label, e),
        }

        match seam_client.create_access_code(event, &code).await {
            Ok(()) => tracing::info!(
                "{} - ✓ Created code: {} ({})",
                label,
                code,
                fmt_range(event)
            ),
            Err(e) => tracing::error!("{} - ✗ Failed: {}", label, e),
        }
    }

    tracing::info!("✓ Code creation complete");
    Ok(())
}
