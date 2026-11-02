use crate::reconcile::reconcile;
use crate::tz::format_local;
use crate::{google, ics, models::Config};

fn fmt_dt(dt: &chrono::DateTime<chrono::Utc>) -> String {
    format_local(dt, "%Y-%m-%d %H:%M")
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

    // 3. Reconcile (shared with `fix-times`, so the two never disagree)
    let report = reconcile(&expected, &gcal_events);

    for event in &report.ok {
        tracing::info!(
            "OK: {} ({} → {})",
            event.reservation_id,
            fmt_dt(&event.checkin),
            fmt_dt(&event.checkout)
        );
    }

    for event in &report.missing {
        tracing::warn!(
            "MISSING in Google Calendar: {} ({} → {})",
            event.reservation_id,
            fmt_dt(&event.checkin),
            fmt_dt(&event.checkout)
        );
    }

    for m in &report.mismatched {
        tracing::warn!(
            "TIME MISMATCH: {} | ICS: {} → {} | GCal: {} → {}",
            m.reservation_id,
            fmt_dt(&m.expected.checkin),
            fmt_dt(&m.expected.checkout),
            fmt_dt(&m.actual.checkin),
            fmt_dt(&m.actual.checkout),
        );
    }

    for event in &report.extra {
        tracing::warn!(
            "EXTRA in Google Calendar (not in ICS): {} \"{}\" ({} → {})",
            event.booking_id.as_deref().unwrap_or(&event.reservation_id),
            event.title,
            fmt_dt(&event.checkin),
            fmt_dt(&event.checkout),
        );
    }

    // 4. Summary
    let issues = report.issue_count();
    if issues == 0 {
        tracing::info!(
            "✓ Audit passed — all {} reservations in sync",
            expected.len()
        );
    } else {
        tracing::warn!(
            "✗ Audit found {} issue(s) across {} reservations ({} missing, {} mismatched, {} extra). Run `fix-times` to correct mismatches.",
            issues,
            expected.len(),
            report.missing.len(),
            report.mismatched.len(),
            report.extra.len(),
        );
    }

    Ok(())
}
