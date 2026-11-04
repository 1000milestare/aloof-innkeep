//! One-off correction script for the DST offset bug: finds Google Calendar
//! events whose checkin/checkout no longer match what the ICS feed says
//! they should be (per `reconcile`), and patches them to the correct time.
//!
//! Safe by default — without `--apply` this only reports what it *would*
//! change, same as the other commands' `--dry-run`, but inverted: you have
//! to opt in to writes rather than opt out, since this command's entire
//! purpose is bulk-editing existing calendar events.

use crate::reconcile::reconcile;
use crate::tz::format_local;
use crate::{google, ics, models::Config};

fn fmt_dt(dt: &chrono::DateTime<chrono::Utc>) -> String {
    format_local(dt, "%Y-%m-%d %H:%M")
}

pub async fn fix_times_command(
    config: &Config,
    apply: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if apply {
        tracing::info!("Starting fix-times — APPLY mode, events will be updated");
    } else {
        tracing::info!("Starting fix-times — preview mode (pass --apply to write changes)");
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

    let report = reconcile(&expected, &gcal_events);

    if report.mismatched.is_empty() {
        tracing::info!("✓ No time mismatches found — nothing to fix");
        if !report.missing.is_empty() || !report.extra.is_empty() {
            tracing::info!(
                "(Note: {} missing and {} extra reservations exist but are out of scope for \
                 fix-times — missing events need `sync`, extra events need manual review.)",
                report.missing.len(),
                report.extra.len()
            );
        }
        return Ok(());
    }

    let mut fixed = 0;
    let mut failed = 0;

    for m in &report.mismatched {
        let gcal_event_id = &m.actual.reservation_id;

        if apply {
            match google_client
                .update_event_times(gcal_event_id, m.expected.checkin, m.expected.checkout)
                .await
            {
                Ok(()) => {
                    tracing::info!(
                        "✓ Fixed {} | {} → {} (was {} → {})",
                        m.reservation_id,
                        fmt_dt(&m.expected.checkin),
                        fmt_dt(&m.expected.checkout),
                        fmt_dt(&m.actual.checkin),
                        fmt_dt(&m.actual.checkout),
                    );
                    fixed += 1;
                }
                Err(e) => {
                    tracing::error!("✗ Failed to fix {}: {}", m.reservation_id, e);
                    failed += 1;
                }
            }
        } else {
            tracing::info!(
                "Would fix {} | {} → {} (currently {} → {})",
                m.reservation_id,
                fmt_dt(&m.expected.checkin),
                fmt_dt(&m.expected.checkout),
                fmt_dt(&m.actual.checkin),
                fmt_dt(&m.actual.checkout),
            );
        }
    }

    if apply {
        tracing::info!(
            "✓ fix-times complete — {} fixed, {} failed, {} total mismatches",
            fixed,
            failed,
            report.mismatched.len()
        );
    } else {
        tracing::info!(
            "Preview complete — {} event(s) would be corrected. Re-run with --apply to write changes.",
            report.mismatched.len()
        );
    }

    Ok(())
}
