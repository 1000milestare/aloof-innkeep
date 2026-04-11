use crate::event_filter;
use crate::models::CalendarEvent;
use anyhow::{anyhow, Result};
use chrono::{Local, Utc};
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

    pub async fn create_event(&self, event: &CalendarEvent) -> Result<String> {
        tracing::info!(
            "Creating calendar event for reservation: {}",
            event.reservation_id
        );

        let client = reqwest::Client::new();

        // Check if event already exists
        let search_url = format!(
            "https://www.googleapis.com/calendar/v3/calendars/{}/events?q={}",
            urlencoding::encode(&self.calendar_id),
            urlencoding::encode(&event.reservation_id)
        );

        let search_response = client
            .get(&search_url)
            .header("Authorization", format!("Bearer {}", self.access_token))
            .send()
            .await?;

        if search_response.status().is_success() {
            let search_body: serde_json::Value = search_response.json().await?;
            if let Some(items) = search_body.get("items").and_then(|v| v.as_array()) {
                if !items.is_empty() {
                    tracing::info!("Event {} already exists, skipping", event.reservation_id);
                    return Ok(items[0]["id"].as_str().unwrap_or("existing").to_string());
                }
            }
        }

        let ical_uid = format!("{}@airbnb-sync.local", &event.reservation_id);
        let event_body = json!({
            "summary": &event.title,
            "description": &event.description,
            "iCalUID": &ical_uid,
            "start": {
                "dateTime": event.checkin.to_rfc3339(),
                "timeZone": "America/Los_Angeles"
            },
            "end": {
                "dateTime": event.checkout.to_rfc3339(),
                "timeZone": "America/Los_Angeles"
            }
        });

        let url = format!(
            "https://www.googleapis.com/calendar/v3/calendars/{}/events",
            urlencoding::encode(&self.calendar_id)
        );

        let response = client
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
