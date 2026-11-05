use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::URL_SAFE, Engine};
use regex::Regex;
use std::collections::HashMap;

const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const GMAIL_API: &str = "https://gmail.googleapis.com/gmail/v1/users/me";
const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const SCOPE: &str = "https://www.googleapis.com/auth/gmail.readonly";
const REDIRECT_URI: &str = "urn:ietf:wg:oauth:2.0:oob";

/// Check-in/checkout times parsed from an Airbnb email.
#[derive(Debug, Clone)]
pub struct ReservationTimes {
    pub checkin_time: String,  // e.g. "15:00"
    pub checkout_time: String, // e.g. "10:00"
}

/// Generate the OAuth2 authorization URL for the user to visit.
pub fn auth_url(client_id: &str) -> String {
    format!(
        "{}?client_id={}&redirect_uri={}&response_type=code&scope={}&access_type=offline&prompt=consent",
        AUTH_URL,
        urlencoding::encode(client_id),
        urlencoding::encode(REDIRECT_URI),
        urlencoding::encode(SCOPE),
    )
}

/// Exchange an authorization code for a refresh token.
pub async fn exchange_code(client_id: &str, client_secret: &str, code: &str) -> Result<String> {
    let client = reqwest::Client::new();
    let params = [
        ("client_id", client_id),
        ("client_secret", client_secret),
        ("code", code),
        ("grant_type", "authorization_code"),
        ("redirect_uri", REDIRECT_URI),
    ];

    let resp = client.post(TOKEN_URL).form(&params).send().await?;
    if !resp.status().is_success() {
        let err = resp.text().await.unwrap_or_default();
        return Err(anyhow!("Token exchange failed: {}", err));
    }

    let body: serde_json::Value = resp.json().await?;
    body["refresh_token"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow!("No refresh_token in response"))
}

/// Get a fresh access token from a refresh token.
async fn get_access_token(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
) -> Result<String> {
    let client = reqwest::Client::new();
    let params = [
        ("client_id", client_id),
        ("client_secret", client_secret),
        ("refresh_token", refresh_token),
        ("grant_type", "refresh_token"),
    ];

    let resp = client.post(TOKEN_URL).form(&params).send().await?;
    if !resp.status().is_success() {
        let err = resp.text().await.unwrap_or_default();
        return Err(anyhow!("Token refresh failed: {}", err));
    }

    let body: serde_json::Value = resp.json().await?;
    body["access_token"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow!("No access_token in response"))
}

/// Search Gmail for Airbnb reservation emails and extract check-in/checkout times.
/// Returns a map of reservation_id -> ReservationTimes.
pub async fn fetch_reservation_times(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    reservation_ids: &[String],
) -> Result<HashMap<String, ReservationTimes>> {
    let access_token = get_access_token(client_id, client_secret, refresh_token).await?;
    let client = reqwest::Client::new();
    let mut results = HashMap::new();

    for rid in reservation_ids {
        tracing::debug!("Searching Gmail for reservation {}", rid);

        // Search for Airbnb emails containing this reservation ID
        let query = format!("from:automated@airbnb.com {}", rid);
        let search_url = format!(
            "{}/messages?q={}&maxResults=5",
            GMAIL_API,
            urlencoding::encode(&query),
        );

        let resp = client
            .get(&search_url)
            .bearer_auth(&access_token)
            .send()
            .await?;

        if !resp.status().is_success() {
            let err = resp.text().await.unwrap_or_default();
            tracing::warn!("Gmail search failed for {}: {}", rid, err);
            continue;
        }

        let body: serde_json::Value = resp.json().await?;
        let messages = match body["messages"].as_array() {
            Some(msgs) if !msgs.is_empty() => msgs,
            _ => {
                tracing::debug!("No Gmail messages found for {}", rid);
                continue;
            }
        };

        // Fetch all messages and pick the latest one (by internalDate) that has valid times.
        // Gmail API does not document sort order, so we sort explicitly.
        let mut candidates: Vec<(i64, ReservationTimes)> = Vec::new();

        for msg_ref in messages {
            let msg_id = match msg_ref["id"].as_str() {
                Some(id) => id,
                None => continue,
            };

            let msg_url = format!("{}/messages/{}?format=full", GMAIL_API, msg_id);
            let msg_resp = client
                .get(&msg_url)
                .bearer_auth(&access_token)
                .send()
                .await?;

            if !msg_resp.status().is_success() {
                continue;
            }

            let msg: serde_json::Value = msg_resp.json().await?;

            let plaintext = extract_plaintext(&msg);
            if plaintext.is_empty() {
                continue;
            }

            let internal_date = msg["internalDate"]
                .as_str()
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(0);

            if let Some(times) = parse_times_from_email(&plaintext, rid) {
                candidates.push((internal_date, times));
            }
        }

        if let Some(times) = pick_latest_times(candidates) {
            tracing::info!(
                "Found times for {}: check-in {}, checkout {}",
                rid,
                times.checkin_time,
                times.checkout_time
            );
            results.insert(rid.clone(), times);
        }

        if !results.contains_key(rid) {
            tracing::debug!("Could not extract times for {} from any email", rid);
        }
    }

    tracing::info!(
        "Gmail: found check-in/out times for {}/{} reservations",
        results.len(),
        reservation_ids.len()
    );
    Ok(results)
}

/// Choose the most recent set of times. Airbnb sends a new reminder/confirmation
/// email whenever a reservation's check-in or checkout is modified, so the email
/// with the newest `internalDate` (milliseconds since epoch) is authoritative.
fn pick_latest_times(candidates: Vec<(i64, ReservationTimes)>) -> Option<ReservationTimes> {
    candidates
        .into_iter()
        .max_by_key(|(ts, _)| *ts)
        .map(|(_, times)| times)
}

/// Extract the plaintext body from a Gmail message (handles multipart).
fn extract_plaintext(msg: &serde_json::Value) -> String {
    // Try top-level body first
    if let Some(text) = decode_part(&msg["payload"]) {
        return text;
    }

    // Walk multipart parts
    if let Some(parts) = msg["payload"]["parts"].as_array() {
        for part in parts {
            if let Some(mime) = part["mimeType"].as_str() {
                if mime == "text/plain" {
                    if let Some(text) = decode_part(part) {
                        return text;
                    }
                }
            }
            // Nested multipart (e.g., multipart/alternative inside multipart/mixed)
            if let Some(sub_parts) = part["parts"].as_array() {
                for sub in sub_parts {
                    if let Some(mime) = sub["mimeType"].as_str() {
                        if mime == "text/plain" {
                            if let Some(text) = decode_part(sub) {
                                return text;
                            }
                        }
                    }
                }
            }
        }
    }

    String::new()
}

/// Decode a base64url-encoded body part.
fn decode_part(part: &serde_json::Value) -> Option<String> {
    let data = part["body"]["data"].as_str()?;
    let bytes = URL_SAFE.decode(data).ok()?;
    String::from_utf8(bytes).ok()
}

/// Parse check-in/checkout times from the plaintext body of an Airbnb email.
///
/// Expected format in plaintext:
/// ```
/// Check-in      Checkout
/// Fri, Apr 17   Sun, Apr 19
/// 3:00 PM       10:00 AM
/// ```
fn parse_times_from_email(body: &str, reservation_id: &str) -> Option<ReservationTimes> {
    // Verify this email is about the right reservation
    if !body.contains(reservation_id) {
        return None;
    }

    // Match the times line after "Check-in" header
    // The plaintext uses narrow no-break space (U+202F) between number and AM/PM
    // Also handle regular space
    let time_re = Regex::new(
        r"(?i)Check-in\s+Checkout\s+\S.*?\n\s*(\d{1,2}:\d{2}[\s\x{202F}]*[AP]M)\s+(\d{1,2}:\d{2}[\s\x{202F}]*[AP]M)",
    ).ok()?;

    let caps = time_re.captures(body)?;
    let checkin_raw = normalize_time(&caps[1]);
    let checkout_raw = normalize_time(&caps[2]);

    let checkin_24 = to_24h(&checkin_raw)?;
    let checkout_24 = to_24h(&checkout_raw)?;

    Some(ReservationTimes {
        checkin_time: checkin_24,
        checkout_time: checkout_24,
    })
}

/// Normalize whitespace variants (narrow no-break space, etc.) to regular space.
fn normalize_time(s: &str) -> String {
    s.replace(['\u{202F}', '\u{00A0}'], " ")
}

/// Convert "3:00 PM" -> "15:00", "10:00 AM" -> "10:00".
fn to_24h(time_str: &str) -> Option<String> {
    let s = time_str.trim().to_uppercase();
    let is_pm = s.contains("PM");
    let num_part = s
        .trim_end_matches("AM")
        .trim_end_matches("PM")
        .trim()
        .to_string();
    let parts: Vec<&str> = num_part.split(':').collect();
    if parts.len() != 2 {
        return None;
    }
    let mut hour: u32 = parts[0].trim().parse().ok()?;
    let min: u32 = parts[1].trim().parse().ok()?;

    if is_pm && hour != 12 {
        hour += 12;
    } else if !is_pm && hour == 12 {
        hour = 0;
    }

    Some(format!("{:02}:{:02}", hour, min))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_to_24h_pm() {
        assert_eq!(to_24h("3:00 PM"), Some("15:00".to_string()));
    }

    #[test]
    fn test_to_24h_am() {
        assert_eq!(to_24h("10:00 AM"), Some("10:00".to_string()));
    }

    #[test]
    fn test_to_24h_noon() {
        assert_eq!(to_24h("12:00 PM"), Some("12:00".to_string()));
    }

    #[test]
    fn test_to_24h_midnight() {
        assert_eq!(to_24h("12:00 AM"), Some("00:00".to_string()));
    }

    #[test]
    fn test_to_24h_narrow_space() {
        // U+202F narrow no-break space
        assert_eq!(to_24h("3:00\u{202F}PM"), Some("15:00".to_string()));
    }

    #[test]
    fn test_parse_times_from_email() {
        let body = r#"KATE ARRIVES FRIDAY, APR 17.

Check-in      Checkout

Fri, Apr 17   Sun, Apr 19

3:00 PM       10:00 AM

CONFIRMATION CODE
HM4S2ZTZZX
"#;
        let result = parse_times_from_email(body, "HM4S2ZTZZX");
        assert!(result.is_some());
        let times = result.unwrap();
        assert_eq!(times.checkin_time, "15:00");
        assert_eq!(times.checkout_time, "10:00");
    }

    #[test]
    fn test_parse_times_wrong_reservation() {
        let body = "Check-in Checkout\nFri, Apr 17 Sun, Apr 19\n3:00 PM 10:00 AM\nCONFIRMATION CODE\nHM4S2ZTZZX";
        let result = parse_times_from_email(body, "WRONGID");
        assert!(result.is_none());
    }

    /// Plaintext body of an Airbnb "Reservation reminder" email, after Gmail has
    /// decoded the quoted-printable transfer encoding. Structure matches the real
    /// template (blank padding lines around the table, U+202F before AM/PM); guest
    /// name, dates and confirmation code are fabricated.
    const REMINDER_FIXTURE: &str = "\
%opentrack%

GUEST ARRIVES WEDNESDAY, SEP 23.

If you haven\u{2019}t already, reach out to Test Guest to send
directions and coordinate check-in time.

Test Guest

Hi Host, we're coming for the Example Faire!

BAYVIEW MANSE MIDWAY BETWEEN NAPA & SAN FRANCISCO

Entire home/apt

Check-in      Checkout
             \n\
Wed, Sep 23   Mon, Sep 28
             \n\
3:00\u{202F}PM       10:00\u{202F}AM

GUESTS

5 adults

CONFIRMATION CODE
TESTRES0001

GUEST PAID
";

    #[test]
    fn test_parse_times_from_reminder_template() {
        let times = parse_times_from_email(REMINDER_FIXTURE, "TESTRES0001").unwrap();
        assert_eq!(times.checkin_time, "15:00");
        assert_eq!(times.checkout_time, "10:00");
    }

    #[test]
    fn test_parse_times_from_reminder_modified_checkin_and_checkout() {
        // A host-modified reservation: the same template, with non-default times.
        // Checkout at 12:00 PM must map to noon (12:00), not midnight.
        let body = REMINDER_FIXTURE
            .replace("3:00\u{202F}PM", "1:00\u{202F}PM")
            .replace("10:00\u{202F}AM", "12:00\u{202F}PM");
        let times = parse_times_from_email(&body, "TESTRES0001").unwrap();
        assert_eq!(times.checkin_time, "13:00");
        assert_eq!(times.checkout_time, "12:00");
    }

    #[test]
    fn test_parse_times_reminder_for_other_reservation_is_ignored() {
        assert!(parse_times_from_email(REMINDER_FIXTURE, "TESTRES9999").is_none());
    }

    #[test]
    fn test_parse_times_reminder_without_table_returns_none() {
        // A message that mentions the reservation but has no check-in table
        // (e.g. a plain guest message) must not produce times.
        let body = "Hi, can I check in early?\nCONFIRMATION CODE\nTESTRES0001\n";
        assert!(parse_times_from_email(body, "TESTRES0001").is_none());
    }

    #[test]
    fn test_pick_latest_times_prefers_newest_email_regardless_of_order() {
        let original = ReservationTimes {
            checkin_time: "15:00".into(),
            checkout_time: "10:00".into(),
        };
        let modified = ReservationTimes {
            checkin_time: "13:00".into(),
            checkout_time: "12:00".into(),
        };
        // The modification arrives later (higher internalDate) but is listed first.
        let picked = pick_latest_times(vec![
            (1_789_600_000_000, modified.clone()),
            (1_789_500_000_000, original),
        ])
        .unwrap();
        assert_eq!(picked.checkin_time, modified.checkin_time);
        assert_eq!(picked.checkout_time, modified.checkout_time);
    }

    #[test]
    fn test_pick_latest_times_empty_is_none() {
        assert!(pick_latest_times(vec![]).is_none());
    }
}
