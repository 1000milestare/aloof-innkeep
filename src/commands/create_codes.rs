use crate::models::CalendarEvent;
use crate::tz::format_local;
use crate::{event_filter, google, models::Config, seam};

/// Format checkin/checkout as "YYYY-MM-DD HH:MM" local time for logging.
fn fmt_range(event: &CalendarEvent) -> String {
    format!(
        "{} → {}",
        format_local(&event.checkin, "%Y-%m-%d %H:%M"),
        format_local(&event.checkout, "%Y-%m-%d %H:%M"),
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
            format_local(&event.checkin, "%Y-%m-%d %H:%M"),
            event_filter::extract_phone(&event.description, &event.title),
            &event.description[..event.description.len().min(200)]
        );

        let phone = event_filter::extract_phone(&event.description, &event.title);

        let Some(code) = phone else {
            tracing::info!("{} - No phone found, skipping", label);
            continue;
        };

        // Read-only lookup, so it runs in dry-run too and the preview is accurate.
        // If we can't read the device state we don't know whether to create or
        // update, so skip this event rather than guess.
        let existing = match seam_client.find_existing_code(event).await {
            Ok(existing) => existing,
            Err(e) => {
                tracing::error!(
                    "{} - ✗ Could not read existing codes, skipping: {}",
                    label,
                    e
                );
                continue;
            }
        };

        match seam::plan_code_action(existing.as_ref(), event, &code) {
            seam::CodeAction::Unchanged => {
                tracing::info!("{} - Code up to date, skipping", label);
            }

            seam::CodeAction::Create => {
                if dry_run {
                    tracing::info!(
                        "{} - Would create code: {} ({})",
                        label,
                        code,
                        fmt_range(event)
                    );
                    continue;
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

            seam::CodeAction::Update {
                access_code_id,
                new_code,
                new_times,
            } => {
                let times_note = match new_times {
                    Some(_) => format!("times -> {}", fmt_range(event)),
                    None => "times unchanged".to_string(),
                };
                let code_note = match &new_code {
                    Some(c) => format!("PIN -> {}", c),
                    None => "PIN unchanged".to_string(),
                };

                if dry_run {
                    tracing::info!(
                        "{} - Would update code ({}, {})",
                        label,
                        code_note,
                        times_note
                    );
                    continue;
                }
                match seam_client
                    .update_access_code(&access_code_id, new_code.as_deref(), new_times)
                    .await
                {
                    Ok(()) => {
                        tracing::info!("{} - ✓ Updated code ({}, {})", label, code_note, times_note)
                    }
                    Err(e) => tracing::error!("{} - ✗ Update failed: {}", label, e),
                }
            }
        }
    }

    tracing::info!("✓ Code creation complete");
    Ok(())
}
