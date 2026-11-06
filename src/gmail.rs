use crate::mailbox::{bare_address, Mail};
use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::URL_SAFE, Engine};
use regex::Regex;

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
    /// Guest breakdown from the reminder's GUESTS section, e.g. "5 adults".
    pub guest_count: Option<String>,
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

/// Airbnb reminder emails and owner command emails for these reservations, as
/// `Mail`. Parsing happens in `mailbox`, shared with the KV store.
pub async fn fetch_mail_for_reservations(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    command_sender: &str,
    reservation_ids: &[String],
) -> Result<Vec<Mail>> {
    let access_token = get_access_token(client_id, client_secret, refresh_token).await?;
    let client = reqwest::Client::new();
    let mut out = Vec::new();

    for rid in reservation_ids {
        out.extend(search_messages(&client, &access_token, "from:automated@airbnb.com", rid).await);
        if !command_sender.is_empty() {
            let query = format!("from:{}", command_sender);
            out.extend(search_messages(&client, &access_token, &query, rid).await);
        }
    }
    Ok(out)
}

/// Reservation code from an Airbnb cancellation subject, e.g.
/// "Canceled: Reservation HMFWJXSQRM for Sep 25–28, 2025" → "HMFWJXSQRM".
/// Returns None for anything else (reminders, confirmations, other Airbnb mail).
pub fn parse_cancellation_subject(subject: &str) -> Option<String> {
    let re = Regex::new(r"^Canceled: Reservation ([A-Z0-9]{8,12}) for ").ok()?;
    re.captures(subject.trim())
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}

/// Reservation codes named in Airbnb cancellation emails from the last
/// `lookback_days` days. Emails are the only cancellation signal we can rely on
/// (there is no Airbnb API), so callers treat each code as a confirmed cancel.
pub async fn fetch_cancelled_reservation_ids(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    lookback_days: u32,
) -> Result<std::collections::BTreeSet<String>> {
    let access_token = get_access_token(client_id, client_secret, refresh_token).await?;
    let client = reqwest::Client::new();

    let query = format!(
        "from:automated@airbnb.com subject:\"Canceled: Reservation\" newer_than:{}d",
        lookback_days
    );
    let search_url = format!(
        "{}/messages?q={}&maxResults=100",
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
        return Err(anyhow!("Gmail cancellation search failed: {}", err));
    }

    let body: serde_json::Value = resp.json().await?;
    let mut ids = std::collections::BTreeSet::new();
    let refs = body["messages"].as_array().cloned().unwrap_or_default();

    for msg_ref in &refs {
        let Some(msg_id) = msg_ref["id"].as_str() else {
            continue;
        };
        // Metadata only: the subject carries the code, and the body (which names
        // the guest) is never needed or fetched.
        let url = format!(
            "{}/messages/{}?format=metadata&metadataHeaders=Subject",
            GMAIL_API, msg_id
        );
        let msg: serde_json::Value = client
            .get(&url)
            .bearer_auth(&access_token)
            .send()
            .await?
            .json()
            .await?;

        let subject = msg["payload"]["headers"]
            .as_array()
            .and_then(|hs| {
                hs.iter()
                    .find(|h| h["name"].as_str() == Some("Subject"))
                    .and_then(|h| h["value"].as_str())
            })
            .unwrap_or("");

        match parse_cancellation_subject(subject) {
            Some(code) => {
                tracing::info!("Cancellation email found for {}", code);
                ids.insert(code);
            }
            None => tracing::warn!("Unrecognised cancellation subject, ignoring: {}", subject),
        }
    }

    Ok(ids)
}

/// Messages matching `prefix` plus the reservation ID, as `Mail`. Errors are logged
/// and yield nothing, so one failed lookup falls back to defaults, not a failed run.
async fn search_messages(
    client: &reqwest::Client,
    token: &str,
    prefix: &str,
    rid: &str,
) -> Vec<Mail> {
    let query = format!("{} {}", prefix, rid);
    let search_url = format!(
        "{}/messages?q={}&maxResults=5",
        GMAIL_API,
        urlencoding::encode(&query),
    );

    let body: serde_json::Value = match client.get(&search_url).bearer_auth(token).send().await {
        Ok(resp) if resp.status().is_success() => match resp.json().await {
            Ok(body) => body,
            Err(_) => return Vec::new(),
        },
        Ok(resp) => {
            let err = resp.text().await.unwrap_or_default();
            tracing::warn!("Gmail search failed for {}: {}", rid, err);
            return Vec::new();
        }
        Err(e) => {
            tracing::warn!("Gmail search failed for {}: {}", rid, e);
            return Vec::new();
        }
    };

    let mut out = Vec::new();
    for msg_ref in body["messages"].as_array().cloned().unwrap_or_default() {
        let Some(msg_id) = msg_ref["id"].as_str() else {
            continue;
        };
        let url = format!("{}/messages/{}?format=full", GMAIL_API, msg_id);
        let Ok(resp) = client.get(&url).bearer_auth(token).send().await else {
            continue;
        };
        if !resp.status().is_success() {
            continue;
        }
        let Ok(msg) = resp.json::<serde_json::Value>().await else {
            continue;
        };
        let text = extract_plaintext(&msg);
        if text.is_empty() {
            continue;
        }
        out.push(Mail {
            from: bare_address(&gmail_header(&msg, "From")),
            subject: gmail_header(&msg, "Subject"),
            received_ms: msg["internalDate"]
                .as_str()
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(0),
            text,
        });
    }
    out
}

/// A header value from a Gmail API message payload.
fn gmail_header(msg: &serde_json::Value, name: &str) -> String {
    msg["payload"]["headers"]
        .as_array()
        .and_then(|hs| {
            hs.iter().find(|h| {
                h["name"]
                    .as_str()
                    .is_some_and(|n| n.eq_ignore_ascii_case(name))
            })
        })
        .and_then(|h| h["value"].as_str())
        .unwrap_or("")
        .to_string()
}

/// A check-in or checkout change written by the owner, e.g.
/// "checkout HMFWJXSQRM 13:00" or "checkin HMFWJXSQRM 3:00 PM".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimeCommand {
    CheckIn(String),  // "HH:MM"
    CheckOut(String), // "HH:MM"
}

/// Every well-formed command line for this reservation in a message body. Lines
/// that don't match the grammar exactly are ignored, so ordinary mail can't
/// change times by accident.
pub fn parse_time_commands(body: &str, reservation_id: &str) -> Vec<TimeCommand> {
    let pattern = format!(
        r"(?i)^\s*(checkin|check-in|checkout|check-out)\s+{}\s+(\d{{1,2}}:\d{{2}}(?:\s*[ap]m)?)\s*$",
        regex::escape(reservation_id)
    );
    let Ok(re) = Regex::new(&pattern) else {
        return Vec::new();
    };

    body.lines()
        .filter_map(|line| {
            let caps = re.captures(line)?;
            let time = parse_clock(&caps[2])?;
            match caps[1].to_lowercase().as_str() {
                "checkin" | "check-in" => Some(TimeCommand::CheckIn(time)),
                _ => Some(TimeCommand::CheckOut(time)),
            }
        })
        .collect()
}

/// "13:00", "3:00 PM", "12:30am" → "HH:MM" (24-hour). None if the time is not real.
fn parse_clock(s: &str) -> Option<String> {
    let upper = s.trim().to_uppercase();
    let (h, m) = if upper.ends_with("AM") || upper.ends_with("PM") {
        let converted = to_24h(&upper)?;
        let (h, m) = converted.split_once(':')?;
        (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?)
    } else {
        let (h, m) = upper.split_once(':')?;
        (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?)
    };
    chrono::NaiveTime::from_hms_opt(h, m, 0).map(|t| t.format("%H:%M").to_string())
}

/// Combine reminder emails and owner commands into one set of times. Each field
/// takes its newest value from either source, so a command sent after a reminder
/// wins, and a later reminder wins over an older command. A field with no source
/// uses the config default. The guest count comes from the newest reminder.
pub(crate) fn resolve_times(
    reminders: Vec<(i64, ReservationTimes)>,
    commands: Vec<(i64, TimeCommand)>,
    default_checkin: &str,
    default_checkout: &str,
) -> Option<ReservationTimes> {
    if reminders.is_empty() && commands.is_empty() {
        return None;
    }

    let guest_count = reminders
        .iter()
        .max_by_key(|(ts, _)| *ts)
        .and_then(|(_, t)| t.guest_count.clone());

    let checkin = newest(
        reminders
            .iter()
            .map(|(ts, t)| (*ts, t.checkin_time.clone()))
            .chain(commands.iter().filter_map(|(ts, c)| match c {
                TimeCommand::CheckIn(t) => Some((*ts, t.clone())),
                TimeCommand::CheckOut(_) => None,
            })),
    );
    let checkout = newest(
        reminders
            .iter()
            .map(|(ts, t)| (*ts, t.checkout_time.clone()))
            .chain(commands.iter().filter_map(|(ts, c)| match c {
                TimeCommand::CheckOut(t) => Some((*ts, t.clone())),
                TimeCommand::CheckIn(_) => None,
            })),
    );

    Some(ReservationTimes {
        checkin_time: checkin.unwrap_or_else(|| default_checkin.to_string()),
        checkout_time: checkout.unwrap_or_else(|| default_checkout.to_string()),
        guest_count,
    })
}

/// The value with the largest timestamp.
fn newest(items: impl Iterator<Item = (i64, String)>) -> Option<String> {
    items.max_by_key(|(ts, _)| *ts).map(|(_, t)| t)
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
pub(crate) fn parse_times_from_email(body: &str, reservation_id: &str) -> Option<ReservationTimes> {
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
        guest_count: parse_guest_count(body),
    })
}

/// Parse the guest breakdown from the GUESTS section of an Airbnb email, e.g.
/// ```text
/// GUESTS
///
/// 5 adults
///
/// CONFIRMATION CODE
/// ```
/// Only a bare `GUESTS` heading is used. `REQUESTED GUESTS` and `ORIGINAL GUESTS`
/// belong to change requests that may not have been accepted, so they are ignored.
/// Wrapped lines ("5 guests,\n2 pets") are joined; the section ends at the first
/// blank line after content or at the next all-caps heading.
fn parse_guest_count(body: &str) -> Option<String> {
    let lines: Vec<&str> = body.lines().map(str::trim).collect();
    let start = lines
        .iter()
        .position(|l| l.eq_ignore_ascii_case("GUESTS"))?;

    let mut parts: Vec<&str> = Vec::new();
    for line in &lines[start + 1..] {
        if line.is_empty() {
            if parts.is_empty() {
                continue;
            }
            break;
        }
        let is_heading = line.chars().any(|c| c.is_alphabetic())
            && !line.chars().any(|c| c.is_ascii_digit())
            && *line == line.to_uppercase();
        if is_heading {
            break;
        }
        parts.push(line.trim_end_matches(','));
    }

    let joined = parts.join(", ");
    // A real breakdown always starts with a number; anything else is stray text.
    if joined.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        Some(joined)
    } else {
        None
    }
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

    fn times(checkin: &str, checkout: &str) -> ReservationTimes {
        ReservationTimes {
            checkin_time: checkin.into(),
            checkout_time: checkout.into(),
            guest_count: None,
        }
    }

    #[test]
    fn command_accepts_24h_and_12h_forms() {
        let body = "checkout HMTEST12345 13:00\ncheckin HMTEST12345 3:00 PM\n";
        assert_eq!(
            parse_time_commands(body, "HMTEST12345"),
            vec![
                TimeCommand::CheckOut("13:00".into()),
                TimeCommand::CheckIn("15:00".into()),
            ]
        );
    }

    #[test]
    fn command_ignores_other_reservations_and_prose() {
        let body = "checkout HMOTHER999 13:00\nPlease can we check out later?\ncheckout HMTEST12345 13:00 extra";
        assert!(parse_time_commands(body, "HMTEST12345").is_empty());
    }

    #[test]
    fn command_rejects_impossible_times() {
        assert!(parse_time_commands("checkout HMTEST12345 13:00 PM", "HMTEST12345").is_empty());
        assert!(parse_time_commands("checkout HMTEST12345 25:00", "HMTEST12345").is_empty());
    }

    #[test]
    fn newer_command_beats_older_reminder() {
        let reminders = vec![(100, times("15:00", "10:00"))];
        let commands = vec![(200, TimeCommand::CheckOut("13:00".into()))];
        let r = resolve_times(reminders, commands, "15:00", "10:00").unwrap();
        assert_eq!(r.checkin_time, "15:00");
        assert_eq!(r.checkout_time, "13:00");
    }

    #[test]
    fn newer_reminder_beats_older_command() {
        let reminders = vec![(300, times("15:00", "12:00"))];
        let commands = vec![(200, TimeCommand::CheckOut("13:00".into()))];
        let r = resolve_times(reminders, commands, "15:00", "10:00").unwrap();
        assert_eq!(r.checkout_time, "12:00");
    }

    #[test]
    fn command_only_fills_the_other_field_from_defaults() {
        let commands = vec![(200, TimeCommand::CheckIn("13:00".into()))];
        let r = resolve_times(Vec::new(), commands, "15:00", "10:00").unwrap();
        assert_eq!(r.checkin_time, "13:00");
        assert_eq!(r.checkout_time, "10:00");
    }

    #[test]
    fn nothing_to_resolve_is_none() {
        assert!(resolve_times(Vec::new(), Vec::new(), "15:00", "10:00").is_none());
    }

    #[test]
    fn cancellation_subject_yields_code() {
        assert_eq!(
            parse_cancellation_subject("Canceled: Reservation HMTEST12345 for Sep 25–28, 2025"),
            Some("HMTEST12345".to_string())
        );
    }

    #[test]
    fn cancellation_subject_across_months_yields_code() {
        assert_eq!(
            parse_cancellation_subject("Canceled: Reservation HMTEST67890 for Oct 30–Nov 2, 2026"),
            Some("HMTEST67890".to_string())
        );
    }

    #[test]
    fn non_cancellation_subjects_are_ignored() {
        assert_eq!(
            parse_cancellation_subject("Reservation reminder: Test Guest is coming soon"),
            None
        );
        assert_eq!(
            parse_cancellation_subject("Reservation confirmed: HMTEST12345"),
            None
        );
        assert_eq!(
            parse_cancellation_subject("Canceled: Reservation  for Sep 1"),
            None
        );
        assert_eq!(parse_cancellation_subject(""), None);
    }

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
    fn test_guest_count_from_reminder_template() {
        let times = parse_times_from_email(REMINDER_FIXTURE, "TESTRES0001").unwrap();
        assert_eq!(times.guest_count.as_deref(), Some("5 adults"));
    }

    #[test]
    fn test_parse_guest_count_joins_wrapped_lines() {
        let body = "GUESTS\n\n4 adults,\n1 child,\n2 pets\n\nCONFIRMATION CODE\nX";
        assert_eq!(
            parse_guest_count(body).as_deref(),
            Some("4 adults, 1 child, 2 pets")
        );
    }

    #[test]
    fn test_parse_guest_count_stops_at_next_heading() {
        let body = "GUESTS\n9 adults, 1 infant\nCONFIRMATION CODE\nTESTRES0001";
        assert_eq!(
            parse_guest_count(body).as_deref(),
            Some("9 adults, 1 infant")
        );
    }

    #[test]
    fn test_parse_guest_count_ignores_unconfirmed_change_requests() {
        let body = "ORIGINAL GUESTS\n5 guests\n\nREQUESTED GUESTS\n13 guests\n\nIf you accept";
        assert_eq!(parse_guest_count(body), None);
    }

    #[test]
    fn test_parse_guest_count_missing_or_non_numeric() {
        assert_eq!(parse_guest_count("no section here"), None);
        assert_eq!(parse_guest_count("GUESTS\n\nCONFIRMATION CODE\nX"), None);
        assert_eq!(parse_guest_count("GUESTS\nsee details in the app\n"), None);
    }

    #[test]
    fn test_pick_latest_times_prefers_newest_email_regardless_of_order() {
        let original = ReservationTimes {
            checkin_time: "15:00".into(),
            checkout_time: "10:00".into(),
            guest_count: None,
        };
        let modified = ReservationTimes {
            checkin_time: "13:00".into(),
            checkout_time: "12:00".into(),
            guest_count: None,
        };
        // The modification arrives later (higher internalDate) but is listed first.
        let picked = resolve_times(
            vec![
                (1_789_600_000_000, modified.clone()),
                (1_789_500_000_000, original),
            ],
            Vec::new(),
            "15:00",
            "10:00",
        )
        .unwrap();
        assert_eq!(picked.checkin_time, modified.checkin_time);
        assert_eq!(picked.checkout_time, modified.checkout_time);
    }

    #[test]
    fn test_resolve_times_empty_is_none() {
        assert!(resolve_times(Vec::new(), Vec::new(), "15:00", "10:00").is_none());
    }
}
