use crate::models::{CalendarEvent, IcsReservation};
use anyhow::{anyhow, Result};
use chrono::{FixedOffset, NaiveDate, NaiveTime, TimeZone, Utc};
use regex::Regex;

pub async fn fetch_ics(url: &str) -> Result<String> {
    tracing::info!("Fetching ICS from: {}", url);
    let client = reqwest::Client::new();
    let response = client.get(url).send().await?;
    if !response.status().is_success() {
        return Err(anyhow!("Failed to fetch ICS: {}", response.status()));
    }
    let content = response.text().await?;
    tracing::info!("Fetched ICS ({} bytes)", content.len());
    Ok(content)
}

pub fn parse_reservations(ics_content: &str) -> Result<Vec<IcsReservation>> {
    let mut reservations = Vec::new();
    let lines: Vec<&str> = ics_content.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        if lines[i] == "BEGIN:VEVENT" {
            let mut event_lines = Vec::new();
            i += 1;
            while i < lines.len() && lines[i] != "END:VEVENT" {
                event_lines.push(lines[i]);
                i += 1;
            }
            if let Ok(res) = parse_vevent(&event_lines) {
                reservations.push(res);
            }
        }
        i += 1;
    }

    tracing::info!("Parsed {} reservations from ICS", reservations.len());
    Ok(reservations)
}

fn parse_vevent(lines: &[&str]) -> Result<IcsReservation> {
    let mut summary = String::new();
    let mut dtstart = String::new();
    let mut dtend = String::new();
    let mut description = String::new();

    for line in lines {
        if line.starts_with("SUMMARY:") {
            summary = line.strip_prefix("SUMMARY:").unwrap_or("").to_string();
        } else if line.starts_with("DTSTART") {
            dtstart = extract_date(line);
        } else if line.starts_with("DTEND") {
            dtend = extract_date(line);
        } else if line.starts_with("DESCRIPTION:") {
            description = line.strip_prefix("DESCRIPTION:").unwrap_or("").to_string();
        } else if line.starts_with(' ') && !description.is_empty() {
            description.push_str(line.trim());
        }
    }

    if !summary.contains("Reserved") {
        return Err(anyhow!("Not a reserved event"));
    }

    let id_regex = Regex::new(r"details/([A-Z0-9]+)")?;
    let reservation_id = id_regex
        .captures(&description)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string())
        .ok_or_else(|| anyhow!("No reservation ID found"))?;

    let url_regex = Regex::new(r"(https://\S+/hosting/reservations/details/[A-Z0-9]+)")?;
    let reservation_url = url_regex
        .captures(&description)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string());

    let phone_regex = Regex::new(r"Phone Number \(Last 4 Digits\): (\d{4})")?;
    let phone_last_4 = phone_regex
        .captures(&description)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string());

    let checkin = NaiveDate::parse_from_str(&dtstart, "%Y%m%d")?;
    let checkout = NaiveDate::parse_from_str(&dtend, "%Y%m%d")?;

    Ok(IcsReservation {
        reservation_id,
        checkin_date: checkin,
        checkout_date: checkout,
        reservation_url,
        phone_last_4,
    })
}

fn extract_date(line: &str) -> String {
    line.find(':')
        .map(|i| line[i + 1..].to_string())
        .unwrap_or_default()
}

pub fn transform_to_calendar_events(
    reservations: Vec<IcsReservation>,
    checkin_time: &str,
    checkout_time: &str,
) -> Result<Vec<CalendarEvent>> {
    let checkin_time_parsed = NaiveTime::parse_from_str(checkin_time, "%H:%M")?;
    let checkout_time_parsed = NaiveTime::parse_from_str(checkout_time, "%H:%M")?;
    let pdt = FixedOffset::west_opt(7 * 3600).unwrap();
    let mut events = Vec::new();

    for res in &reservations {
        let checkin_naive = chrono::NaiveDateTime::new(res.checkin_date, checkin_time_parsed);
        let checkout_naive = chrono::NaiveDateTime::new(res.checkout_date, checkout_time_parsed);

        let checkin = pdt
            .from_local_datetime(&checkin_naive)
            .single()
            .ok_or_else(|| anyhow!("Invalid checkin datetime"))?
            .with_timezone(&Utc);
        let checkout = pdt
            .from_local_datetime(&checkout_naive)
            .single()
            .ok_or_else(|| anyhow!("Invalid checkout datetime"))?
            .with_timezone(&Utc);

        let mut description = format!("Reservation: {}", res.reservation_id);
        if let Some(url) = &res.reservation_url {
            description.push_str(&format!("\n\nReservation URL: {}", url));
        }
        if let Some(phone) = &res.phone_last_4 {
            description.push_str(&format!("\nPhone (Last 4): {}", phone));
        }

        events.push(CalendarEvent {
            title: format!("Reserved: {}", res.reservation_id),
            description,
            checkin,
            checkout,
            reservation_id: res.reservation_id.clone(),
            booking_id: Some(res.reservation_id.clone()),
        });
    }

    tracing::info!(
        "Transformed {} reservations to calendar events",
        reservations.len()
    );
    Ok(events)
}
