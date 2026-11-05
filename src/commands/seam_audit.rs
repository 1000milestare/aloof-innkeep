//! Read-only comparison of every code on the lock against the calendar.

use crate::tz::format_local;
use crate::{google, models::Config, seam};

fn fmt_dt(dt: Option<chrono::DateTime<chrono::Utc>>) -> String {
    dt.map(|d| format_local(&d, "%Y-%m-%d %H:%M"))
        .unwrap_or_else(|| "-".to_string())
}

pub async fn seam_audit_command(config: &Config) -> Result<(), Box<dyn std::error::Error>> {
    tracing::info!("Starting Seam audit — read-only lock code check");

    if config.seam_api_key.is_empty() || config.seam_device_id.is_empty() {
        return Err("Seam API not configured (SEAM_API_KEY / SEAM_DEVICE_ID)".into());
    }

    let google_client = google::GoogleCalendarClient::new(
        config.google_calendar_id.clone(),
        &config.google_service_account_json,
    )
    .await?;
    let events = google_client.list_bookable_events().await?;

    let seam_client =
        seam::SeamClient::new(config.seam_api_key.clone(), config.seam_device_id.clone());
    let codes = seam_client.list_codes().await?;

    tracing::info!(
        "Calendar: {} bookable events | Device: {} codes",
        events.len(),
        codes.len()
    );

    let report = seam::audit_codes(&events, &codes, chrono::Utc::now());

    for e in &report.in_sync {
        tracing::info!(
            "OK: {}",
            e.booking_id.as_deref().unwrap_or(&e.reservation_id)
        );
    }
    for e in &report.needs_create {
        tracing::warn!(
            "MISSING CODE: {} ({} → {})",
            e.booking_id.as_deref().unwrap_or(&e.reservation_id),
            fmt_dt(Some(e.checkin)),
            fmt_dt(Some(e.checkout))
        );
    }
    for (e, action) in &report.needs_update {
        if let seam::CodeAction::Update {
            new_code,
            new_times,
            ..
        } = action
        {
            tracing::warn!(
                "DRIFTED CODE: {} | PIN {} | window {}",
                e.booking_id.as_deref().unwrap_or(&e.reservation_id),
                if new_code.is_some() { "differs" } else { "ok" },
                if new_times.is_some() { "differs" } else { "ok" },
            );
        }
    }
    for e in &report.no_phone {
        tracing::info!(
            "NO PHONE (cannot derive a code): {}",
            e.booking_id.as_deref().unwrap_or(&e.reservation_id)
        );
    }
    for c in &report.orphans {
        tracing::warn!(
            "ORPHAN CODE (no calendar event, not started — likely cancelled): {} ({} → {})",
            c.name.as_deref().unwrap_or("?"),
            fmt_dt(c.starts_at),
            fmt_dt(c.ends_at)
        );
    }
    for c in &report.in_progress {
        tracing::info!(
            "In progress (no future calendar event): {} ({} → {})",
            c.name.as_deref().unwrap_or("?"),
            fmt_dt(c.starts_at),
            fmt_dt(c.ends_at)
        );
    }
    for c in &report.unmanaged {
        tracing::info!(
            "Unmanaged code (no name or no window, ignored): {}",
            c.name.as_deref().unwrap_or("(unnamed)")
        );
    }

    let issues = report.issue_count();
    if issues == 0 {
        tracing::info!("✓ Seam audit passed — lock matches calendar");
    } else {
        tracing::warn!(
            "✗ Seam audit found {} issue(s): {} missing, {} drifted, {} orphan. `create-codes` fixes missing/drifted; `cancel` removes orphans for cancelled reservations.",
            issues,
            report.needs_create.len(),
            report.needs_update.len(),
            report.orphans.len()
        );
    }

    Ok(())
}
