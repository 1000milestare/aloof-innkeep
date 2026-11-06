use crate::buffer::buffer_ical_uid;
use crate::event_filter;
use crate::models::CalendarEvent;
use anyhow::{anyhow, Result};
use chrono::{DateTime, Local, Utc};
use serde_json::json;

pub struct GoogleCalendarClient {
    calendar_id: String,
    access_token: String,
}

impl GoogleCalendarClient {
    pub async fn new(calendar_id: String, service_account_json: &str) -> Result<Self> {
        tracing::info!("Initializing Google Calendar client");

        // If the value looks like JSON (starts with '{'), treat it as inline JSON.
        // Otherwise treat it as a file path for backwards compatibility.
        let json_str = if service_account_json.trim_start().starts_with('{') {
            service_account_json.to_string()
        } else {
            std::fs::read_to_string(service_account_json)?
        };
        let key: serde_json::Value = serde_json::from_str(&json_str)?;

        let client_email = key["client_email"]
            .as_str()
            .ok_or_else(|| anyhow!("Missing client_email in service account key"))?;
        let private_key = key["private_key"]
            .as_str()
            .ok_or_else(|| anyhow!("Missing private_key in service account key"))?;

        let token = generate_jwt(client_email, private_key)?;
        let access_token = exchange_jwt_for_token(&token).await?;

        tracing::info!("Successfully authenticated with Google Calendar API");
        Ok(GoogleCalendarClient {
            calendar_id,
            access_token,
        })
    }

    /// Fetch all events from Google Calendar and return only those that:
    /// - Have "reserved" (case-insensitive) in the title OR have a phone number in the description
    /// - Start today or later (by local date)
    pub async fn list_bookable_events(&self) -> Result<Vec<CalendarEvent>> {
        tracing::info!("Fetching events from Google Calendar");

        let client = reqwest::Client::new();
        let today = Local::now().date_naive();

        // timeMin filters server-side to today-or-later; orderBy=startTime requires it
        let time_min = today.and_hms_opt(0, 0, 0).unwrap().and_utc().to_rfc3339();

        // Paginate: the API returns at most 250 items per page
        let mut all_items: Vec<serde_json::Value> = Vec::new();
        let mut page_token: Option<String> = None;

        loop {
            let mut url = format!(
                "https://www.googleapis.com/calendar/v3/calendars/{}/events?maxResults=250&singleEvents=true&orderBy=startTime&timeMin={}",
                urlencoding::encode(&self.calendar_id),
                urlencoding::encode(&time_min),
            );
            if let Some(ref token) = page_token {
                url.push_str(&format!("&pageToken={}", urlencoding::encode(token)));
            }

            let response = client
                .get(&url)
                .bearer_auth(&self.access_token)
                .send()
                .await?;

            if !response.status().is_success() {
                let error_text = response.text().await.unwrap_or_default();
                return Err(anyhow!("Failed to list events: {}", error_text));
            }

            let body: serde_json::Value = response.json().await?;

            if let Some(items) = body["items"].as_array() {
                all_items.extend(items.clone());
            }

            match body["nextPageToken"].as_str() {
                Some(token) => {
                    tracing::info!("Fetching next page ({} events so far)...", all_items.len());
                    page_token = Some(token.to_string());
                }
                None => break,
            }
        }

        tracing::info!("API returned {} future events", all_items.len());
        let mut events: Vec<CalendarEvent> = Vec::new();

        for item in &all_items {
            let title = item["summary"].as_str().unwrap_or("").to_string();
            let raw_desc = item["description"].as_str().unwrap_or("").to_string();
            let description = unescape_ics_text(&raw_desc);

            // Parse start — try dateTime first, then date-only (all-day events)
            let start_str = item["start"]["dateTime"]
                .as_str()
                .or_else(|| item["start"]["date"].as_str())
                .unwrap_or("");

            let start_date = if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(start_str) {
                dt.with_timezone(&Local).date_naive()
            } else if let Ok(d) = chrono::NaiveDate::parse_from_str(start_str, "%Y-%m-%d") {
                d
            } else {
                tracing::warn!(
                    "Skipping event with unparseable start \"{}\": {}",
                    start_str,
                    title
                );
                continue;
            };

            // Server-side timeMin should handle this, but guard locally too
            if start_date < today {
                continue;
            }

            // Filter: title contains "reserved" (case-insensitive) OR description has a phone
            let is_reserved = title.to_lowercase().contains("reserved");
            let phone = event_filter::extract_phone(&description, &title);
            let has_phone = phone.is_some();

            if !is_reserved && !has_phone {
                tracing::debug!(
                    "Skipping (no reserved/phone): \"{}\" | desc: {:?}",
                    title,
                    &description[..description.len().min(120)]
                );
                continue;
            }

            // Parse full start datetime
            let start_utc = if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(start_str) {
                dt.with_timezone(&Utc)
            } else {
                start_date.and_hms_opt(0, 0, 0).unwrap().and_utc()
            };

            let end_str = item["end"]["dateTime"]
                .as_str()
                .or_else(|| item["end"]["date"].as_str())
                .unwrap_or("");

            let end_utc = if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(end_str) {
                dt.with_timezone(&Utc)
            } else if let Ok(d) = chrono::NaiveDate::parse_from_str(end_str, "%Y-%m-%d") {
                d.and_hms_opt(0, 0, 0).unwrap().and_utc()
            } else {
                tracing::warn!("Could not parse end time for \"{}\": {}", title, end_str);
                continue;
            };

            let id = item["id"].as_str().unwrap_or("unknown").to_string();
            let airbnb_id = event_filter::extract_airbnb_id(&description);

            tracing::debug!(
                "Accepted: \"{}\" | airbnb_id: {:?} | phone: {:?} | gcal_id: {}",
                title,
                airbnb_id,
                phone,
                id
            );

            events.push(CalendarEvent {
                title,
                description,
                reservation_id: id,
                booking_id: airbnb_id,
                checkin: start_utc,
                checkout: end_utc,
            });
        }

        tracing::info!("✓ {} bookable events", events.len());
        Ok(events)
    }

    /// Find the first non-deleted event with exactly this iCalUID, if any.
    async fn find_event_by_ical_uid(&self, ical_uid: &str) -> Result<Option<serde_json::Value>> {
        let url = format!(
            "https://www.googleapis.com/calendar/v3/calendars/{}/events?iCalUID={}",
            urlencoding::encode(&self.calendar_id),
            urlencoding::encode(ical_uid),
        );

        let response = reqwest::Client::new()
            .get(&url)
            .bearer_auth(&self.access_token)
            .send()
            .await?;

        if !response.status().is_success() {
            let error_text = response.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Failed to look up event {}: {}",
                ical_uid,
                error_text
            ));
        }

        let body: serde_json::Value = response.json().await?;
        Ok(body["items"]
            .as_array()
            .and_then(|items| items.first().cloned()))
    }

    async fn insert_event(
        &self,
        title: &str,
        description: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        ical_uid: &str,
    ) -> Result<String> {
        let event_body = json!({
            "summary": title,
            "description": description,
            "iCalUID": ical_uid,
            "start": time_body(start),
            "end": time_body(end),
        });

        let url = format!(
            "https://www.googleapis.com/calendar/v3/calendars/{}/events",
            urlencoding::encode(&self.calendar_id)
        );

        let response = reqwest::Client::new()
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.access_token))
            .json(&event_body)
            .send()
            .await?;

        if !response.status().is_success() {
            let error_text = response.text().await.unwrap_or_default();
            return Err(anyhow!("Failed to create event: {}", error_text));
        }

        let response_body: serde_json::Value = response.json().await?;
        Ok(response_body["id"]
            .as_str()
            .unwrap_or("unknown")
            .to_string())
    }

    /// Bring the booking event in line with the desired state: create it if
    /// missing, otherwise patch whatever has drifted.
    ///
    /// `times_authoritative` is true only when the desired times came from a
    /// reservation reminder email. Default (config) times must never overwrite
    /// an existing event, since they may be replacing a reminder-derived time
    /// that was applied on an earlier run.
    pub async fn sync_booking_event(
        &self,
        event: &CalendarEvent,
        times_authoritative: bool,
    ) -> Result<BookingSync> {
        let ical_uid = format!("{}@airbnb-sync.local", event.reservation_id);

        let Some(existing) = self.find_event_by_ical_uid(&ical_uid).await? else {
            self.insert_event(
                &event.title,
                &event.description,
                event.checkin,
                event.checkout,
                &ical_uid,
            )
            .await?;
            return Ok(BookingSync::Created);
        };

        let Some(body) = booking_update_body(&existing, event, times_authoritative) else {
            return Ok(BookingSync::Unchanged);
        };

        let id = existing["id"].as_str().unwrap_or("").to_string();
        tracing::info!(
            "Updating booking event {} ({})",
            event.reservation_id,
            body.keys().cloned().collect::<Vec<_>>().join(", ")
        );
        let url = format!(
            "https://www.googleapis.com/calendar/v3/calendars/{}/events/{}",
            urlencoding::encode(&self.calendar_id),
            urlencoding::encode(&id),
        );
        let response = reqwest::Client::new()
            .patch(&url)
            .bearer_auth(&self.access_token)
            .json(&serde_json::Value::Object(body))
            .send()
            .await?;

        if !response.status().is_success() {
            let error_text = response.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Failed to update event {}: {}",
                event.reservation_id,
                error_text
            ));
        }
        Ok(BookingSync::Updated)
    }

    /// Create or align the post-checkout cleaning buffer for one booking. The
    /// buffer's times are the source of truth, so an existing buffer is patched
    /// if they have drifted (for example after a checkout-time change).
    pub async fn upsert_cleaning_buffer(
        &self,
        airbnb_id: &str,
        buffer: &CalendarEvent,
    ) -> Result<String> {
        let ical_uid = buffer_ical_uid(airbnb_id);

        let Some(existing) = self.find_event_by_ical_uid(&ical_uid).await? else {
            return self
                .insert_event(
                    &buffer.title,
                    &buffer.description,
                    buffer.checkin,
                    buffer.checkout,
                    &ical_uid,
                )
                .await;
        };

        let id = existing["id"].as_str().unwrap_or("").to_string();
        if time_matches(&existing["start"], buffer.checkin)
            && time_matches(&existing["end"], buffer.checkout)
        {
            return Ok(id);
        }

        tracing::info!("Updating cleaning buffer times for {}", airbnb_id);
        let url = format!(
            "https://www.googleapis.com/calendar/v3/calendars/{}/events/{}",
            urlencoding::encode(&self.calendar_id),
            urlencoding::encode(&id),
        );
        let body = json!({
            "start": time_body(buffer.checkin),
            "end": time_body(buffer.checkout),
        });

        let response = reqwest::Client::new()
            .patch(&url)
            .bearer_auth(&self.access_token)
            .json(&body)
            .send()
            .await?;

        if !response.status().is_success() {
            let error_text = response.text().await.unwrap_or_default();
            return Err(anyhow!("Failed to update cleaning buffer: {}", error_text));
        }
        Ok(id)
    }

    /// Remove the cleaning buffer for a booking, if one exists. Missing is fine.
    pub async fn delete_cleaning_buffer(&self, airbnb_id: &str) -> Result<()> {
        if let Some(existing) = self
            .find_event_by_ical_uid(&buffer_ical_uid(airbnb_id))
            .await?
        {
            if let Some(id) = existing["id"].as_str() {
                self.delete_event(id).await?;
            }
        }
        Ok(())
    }

    /// Delete a Google Calendar event by its Google event ID (not the Airbnb
    /// reservation ID). A 404/410 (already gone) counts as success so retries
    /// after a partial failure are safe.
    pub async fn delete_event(&self, gcal_event_id: &str) -> Result<()> {
        tracing::info!("Deleting Google Calendar event {}", gcal_event_id);

        let url = format!(
            "https://www.googleapis.com/calendar/v3/calendars/{}/events/{}",
            urlencoding::encode(&self.calendar_id),
            urlencoding::encode(gcal_event_id),
        );

        let response = reqwest::Client::new()
            .delete(&url)
            .bearer_auth(&self.access_token)
            .send()
            .await?;

        let status = response.status();
        if status.is_success() || status.as_u16() == 404 || status.as_u16() == 410 {
            return Ok(());
        }

        let text = response.text().await.unwrap_or_default();
        Err(anyhow!(
            "Failed to delete event {} ({}): {}",
            gcal_event_id,
            status,
            text
        ))
    }
}

/// What `sync_booking_event` did for one reservation.
#[derive(Debug, PartialEq, Eq)]
pub enum BookingSync {
    Created,
    Updated,
    Unchanged,
}

/// Build the PATCH body needed to bring an existing booking event in line with
/// `desired`, or `None` if nothing needs changing.
///
/// Times are compared and patched only when `times_authoritative` is set.
/// The guest line is appended if missing, but an existing one is never
/// rewritten, so manual edits to the description survive.
fn booking_update_body(
    existing: &serde_json::Value,
    desired: &CalendarEvent,
    times_authoritative: bool,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    let mut body = serde_json::Map::new();

    if times_authoritative {
        if !time_matches(&existing["start"], desired.checkin) {
            body.insert("start".into(), time_body(desired.checkin));
        }
        if !time_matches(&existing["end"], desired.checkout) {
            body.insert("end".into(), time_body(desired.checkout));
        }
    }

    if let Some(guest_line) = desired
        .description
        .lines()
        .find(|l| l.starts_with("Guests:"))
    {
        // Stored descriptions can hold literal \n sequences, so unescape before checking.
        let current = unescape_ics_text(existing["description"].as_str().unwrap_or(""));
        if !current.lines().any(|l| l.starts_with("Guests:")) {
            let new_description = if current.is_empty() {
                guest_line.to_string()
            } else {
                format!("{}\n{}", current, guest_line)
            };
            body.insert("description".into(), json!(new_description));
        }
    }

    if body.is_empty() {
        None
    } else {
        Some(body)
    }
}

/// Google Calendar time object for an instant, in the calendar's zone.
fn time_body(t: DateTime<Utc>) -> serde_json::Value {
    json!({
        "dateTime": t.to_rfc3339(),
        "timeZone": "America/Los_Angeles"
    })
}

/// True when an event's start/end object is within the reconcile tolerance
/// of the expected instant.
fn time_matches(time: &serde_json::Value, expected: DateTime<Utc>) -> bool {
    time["dateTime"]
        .as_str()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| (dt.with_timezone(&Utc) - expected).num_minutes().abs() <= 1)
        .unwrap_or(false)
}

/// Unescape ICS text property values returned by the Google Calendar API.
/// The API returns descriptions with literal \n (backslash-n) for newlines
/// and \; for semicolons — not actual control characters.
fn unescape_ics_text(input: &str) -> String {
    input
        .replace("\\n", "\n")
        .replace("\\N", "\n")
        .replace("\\;", ";")
        .replace("\\,", ",")
        .replace("\\\\", "\\")
}

/// Unfold RFC 5545 line continuations in raw ICS file content
/// (lines beginning with a space/tab are continuations of the previous line).
/// Not needed for Google Calendar API responses, but kept for completeness.
#[allow(dead_code)]
fn unfold_ics_lines(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    for line in input.lines() {
        if line.starts_with(' ') || line.starts_with('\t') {
            result.push_str(line[1..].trim_end());
        } else {
            if !result.is_empty() {
                result.push('\n');
            }
            result.push_str(line);
        }
    }
    result
}

fn generate_jwt(client_email: &str, private_key: &str) -> Result<String> {
    use jsonwebtoken::{encode, Algorithm, EncodingKey};
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize)]
    struct Claims {
        iss: String,
        scope: String,
        aud: String,
        exp: i64,
        iat: i64,
    }

    let now = Utc::now().timestamp();
    let claims = Claims {
        iss: client_email.to_string(),
        scope: "https://www.googleapis.com/auth/calendar".to_string(),
        aud: "https://oauth2.googleapis.com/token".to_string(),
        exp: now + 3600,
        iat: now,
    };

    let key = EncodingKey::from_rsa_pem(private_key.as_bytes())?;
    let header = jsonwebtoken::Header {
        alg: Algorithm::RS256,
        ..Default::default()
    };
    Ok(encode(&header, &claims, &key)?)
}

async fn exchange_jwt_for_token(jwt: &str) -> Result<String> {
    let client = reqwest::Client::new();

    let params = [
        ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
        ("assertion", jwt),
    ];

    let response = client
        .post("https://oauth2.googleapis.com/token")
        .form(&params)
        .send()
        .await?;

    if !response.status().is_success() {
        let error_text = response.text().await.unwrap_or_default();
        return Err(anyhow!("Failed to authenticate: {}", error_text));
    }

    let body: serde_json::Value = response.json().await?;
    tracing::info!(
        "Successfully obtained access token (expires in {} seconds)",
        body["expires_in"].as_i64().unwrap_or(0)
    );

    body["access_token"]
        .as_str()
        .ok_or_else(|| anyhow!("Missing access_token in response"))
        .map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(day: u32, hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, day, hour, 0, 0).unwrap()
    }

    fn desired(
        checkin: DateTime<Utc>,
        checkout: DateTime<Utc>,
        description: &str,
    ) -> CalendarEvent {
        CalendarEvent {
            title: "Reserved: TEST".into(),
            description: description.into(),
            reservation_id: "HMTEST00001".into(),
            booking_id: Some("HMTEST00001".into()),
            checkin,
            checkout,
        }
    }

    /// A stored event as the Calendar API returns it.
    fn stored(start: DateTime<Utc>, end: DateTime<Utc>, description: &str) -> serde_json::Value {
        json!({
            "id": "gcal-1",
            "description": description,
            "start": time_body(start),
            "end": time_body(end),
        })
    }

    #[test]
    fn unchanged_event_needs_no_patch() {
        let d = desired(at(2, 23), at(5, 18), "Reservation URL: x\nGuests: 2 adults");
        let s = stored(at(2, 23), at(5, 18), "Reservation URL: x\nGuests: 2 adults");
        assert!(booking_update_body(&s, &d, true).is_none());
    }

    #[test]
    fn later_checkout_from_reminder_patches_end_only() {
        let d = desired(at(2, 23), at(5, 20), "Reservation URL: x");
        let s = stored(at(2, 23), at(5, 18), "Reservation URL: x");
        let body = booking_update_body(&s, &d, true).unwrap();
        assert_eq!(body.len(), 1);
        assert!(body.contains_key("end"));
        assert_eq!(body["end"]["dateTime"], json!(at(5, 20).to_rfc3339()));
    }

    #[test]
    fn default_times_never_overwrite_an_existing_event() {
        // Not from a reminder: the stored (possibly reminder-derived) times stay.
        let d = desired(at(2, 23), at(5, 18), "Reservation URL: x");
        let s = stored(at(2, 22), at(5, 20), "Reservation URL: x");
        assert!(booking_update_body(&s, &d, false).is_none());
    }

    #[test]
    fn missing_guest_line_is_appended_once() {
        let d = desired(at(2, 23), at(5, 18), "Reservation URL: x\nGuests: 3 adults");
        let s = stored(at(2, 23), at(5, 18), "Reservation URL: x");
        let body = booking_update_body(&s, &d, true).unwrap();
        assert_eq!(
            body["description"],
            json!("Reservation URL: x\nGuests: 3 adults")
        );
    }

    #[test]
    fn escaped_stored_guest_line_is_recognised() {
        // The API can hand back literal \n sequences; that must not read as "no guest line".
        let d = desired(at(2, 23), at(5, 18), "Reservation URL: x\nGuests: 3 adults");
        let s = stored(
            at(2, 23),
            at(5, 18),
            "Reservation URL: x\\nGuests: 3 adults",
        );
        assert!(booking_update_body(&s, &d, true).is_none());
    }

    #[test]
    fn existing_guest_line_is_not_rewritten() {
        let d = desired(at(2, 23), at(5, 18), "Guests: 4 adults");
        let s = stored(at(2, 23), at(5, 18), "Guests: 2 adults");
        assert!(booking_update_body(&s, &d, true).is_none());
    }
}
