use crate::models::CalendarEvent;
use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use serde_json::json;

/// Access-code times within this many seconds of the calendar are considered equal.
const TIME_TOLERANCE_SECS: i64 = 60;

pub struct SeamClient {
    api_key: String,
    device_id: String,
}

/// An access code already present on the device, as reported by Seam.
#[derive(Debug, Clone, PartialEq)]
pub struct ExistingCode {
    pub access_code_id: String,
    pub name: Option<String>,
    pub code: Option<String>,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
}

/// What `create_codes` should do for one calendar event.
#[derive(Debug, Clone, PartialEq)]
pub enum CodeAction {
    /// No code exists for this reservation yet.
    Create,
    /// A code exists and already matches the calendar.
    Unchanged,
    /// A code exists but its PIN and/or validity window has drifted from the calendar.
    Update {
        access_code_id: String,
        new_code: Option<String>,
        new_times: Option<(DateTime<Utc>, DateTime<Utc>)>,
    },
}

/// Decide what to do for an event given the code (if any) already on the device.
///
/// A field that Seam does not report is treated as "unknown, leave alone" rather
/// than "changed", so a partial response cannot cause an update on every run.
pub fn plan_code_action(
    existing: Option<&ExistingCode>,
    event: &CalendarEvent,
    code: &str,
) -> CodeAction {
    let Some(existing) = existing else {
        return CodeAction::Create;
    };

    let code_changed = existing.code.as_deref().is_some_and(|c| c != code);

    let times_changed = match (existing.starts_at, existing.ends_at) {
        (Some(starts), Some(ends)) => {
            (starts - event.checkin).num_seconds().abs() > TIME_TOLERANCE_SECS
                || (ends - event.checkout).num_seconds().abs() > TIME_TOLERANCE_SECS
        }
        _ => false,
    };

    if !code_changed && !times_changed {
        return CodeAction::Unchanged;
    }

    CodeAction::Update {
        access_code_id: existing.access_code_id.clone(),
        new_code: code_changed.then(|| code.to_string()),
        new_times: times_changed.then_some((event.checkin, event.checkout)),
    }
}

/// Result of comparing every code on the device against the calendar.
#[derive(Debug, Default)]
pub struct SeamAuditReport<'a> {
    /// Calendar events whose code exists and matches.
    pub in_sync: Vec<&'a CalendarEvent>,
    /// Calendar events with a phone number but no code on the device.
    pub needs_create: Vec<&'a CalendarEvent>,
    /// Calendar events whose code has a drifted PIN and/or validity window.
    pub needs_update: Vec<(&'a CalendarEvent, CodeAction)>,
    /// Calendar events with no usable phone number, so no code can be derived.
    pub no_phone: Vec<&'a CalendarEvent>,
    /// Codes that have not started yet and match no calendar event: almost
    /// certainly a cancelled reservation.
    pub orphans: Vec<&'a ExistingCode>,
    /// Codes that are mid-stay (started, not expired) with no future calendar
    /// event. Expected for guests currently in residence, so informational only.
    pub in_progress: Vec<&'a ExistingCode>,
    /// Codes with no name or no validity window (e.g. a permanent owner or
    /// cleaner code). Never touched or flagged.
    pub unmanaged: Vec<&'a ExistingCode>,
}

impl SeamAuditReport<'_> {
    pub fn issue_count(&self) -> usize {
        self.needs_create.len() + self.needs_update.len() + self.orphans.len()
    }
}

/// Compare every code on the device with the calendar. Pure, so it can be tested
/// without a device. `now` decides which unmatched codes are cancelled versus
/// mid-stay.
pub fn audit_codes<'a>(
    events: &'a [CalendarEvent],
    codes: &'a [ExistingCode],
    now: DateTime<Utc>,
) -> SeamAuditReport<'a> {
    let mut report = SeamAuditReport::default();
    let mut matched: std::collections::HashSet<&str> = std::collections::HashSet::new();

    for event in events {
        let name = event.booking_id.as_deref().unwrap_or(&event.reservation_id);
        let existing = codes.iter().find(|c| c.name.as_deref() == Some(name));
        if let Some(c) = existing {
            matched.insert(c.access_code_id.as_str());
        }

        let Some(pin) = crate::event_filter::extract_phone(&event.description, &event.title) else {
            report.no_phone.push(event);
            continue;
        };

        match plan_code_action(existing, event, &pin) {
            CodeAction::Unchanged => report.in_sync.push(event),
            CodeAction::Create => report.needs_create.push(event),
            update @ CodeAction::Update { .. } => report.needs_update.push((event, update)),
        }
    }

    for code in codes {
        if matched.contains(code.access_code_id.as_str()) {
            continue;
        }
        match (&code.name, code.starts_at, code.ends_at) {
            (Some(_), Some(starts), Some(ends)) => {
                if ends <= now {
                    // Expired: nothing to do.
                } else if starts > now {
                    report.orphans.push(code);
                } else {
                    report.in_progress.push(code);
                }
            }
            _ => report.unmanaged.push(code),
        }
    }

    report
}

/// Parse one entry from Seam's `access_codes/list` response.
fn parse_existing_code(value: &serde_json::Value) -> Option<ExistingCode> {
    let parse_ts = |key: &str| {
        value[key]
            .as_str()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
    };

    Some(ExistingCode {
        access_code_id: value["access_code_id"].as_str()?.to_string(),
        name: value["name"].as_str().map(str::to_string),
        code: value["code"].as_str().map(str::to_string),
        starts_at: parse_ts("starts_at"),
        ends_at: parse_ts("ends_at"),
    })
}

impl SeamClient {
    pub fn new(api_key: String, device_id: String) -> Self {
        SeamClient { api_key, device_id }
    }

    /// The name used for the Seam access code — Airbnb reservation ID when available,
    /// otherwise the Google Calendar event ID.
    fn code_name(event: &CalendarEvent) -> &str {
        event.booking_id.as_deref().unwrap_or(&event.reservation_id)
    }

    pub async fn create_access_code(&self, event: &CalendarEvent, code: &str) -> Result<()> {
        let name = Self::code_name(event);
        tracing::info!("Creating access code \"{}\" ({})", name, code);

        let request_body = json!({
            "device_id": &self.device_id,
            "name": name,
            "code": code,
            "is_offline_access_code": false,
            "is_one_time_use": false,
            "starts_at": event.checkin.to_rfc3339(),
            "ends_at": event.checkout.to_rfc3339()
        });

        let client = reqwest::Client::new();
        let response = client
            .post("https://connect.getseam.com/access_codes/create")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&request_body)
            .send()
            .await?;

        let status = response.status();
        let body: serde_json::Value = response.json().await.unwrap_or_else(|_| json!({}));

        if status.is_success() {
            let code_id = body["access_code"]["access_code_id"]
                .as_str()
                .unwrap_or("unknown");
            tracing::info!("  ✓ Created (id: {})", code_id);
            return Ok(());
        }

        // Distinguish duplicate (already exists — not our problem) from real errors
        let error_type = body["error"]["type"].as_str().unwrap_or("");
        if error_type == "duplicate_access_code" {
            tracing::warn!("  Code already exists on device (duplicate PIN), skipping");
            return Ok(());
        }

        let message = body["error"]["message"]
            .as_str()
            .unwrap_or_else(|| body.to_string().leak());
        Err(anyhow!("Seam error ({}): {}", error_type, message))
    }

    /// Change the PIN and/or validity window of an existing access code.
    ///
    /// Seam's docs show this endpoint as `PATCH` in the reference but `POST` in
    /// the example; `POST` is used here to match the rest of this client. A wrong
    /// method fails with a non-2xx response rather than changing anything.
    pub async fn update_access_code(
        &self,
        access_code_id: &str,
        new_code: Option<&str>,
        new_times: Option<(DateTime<Utc>, DateTime<Utc>)>,
    ) -> Result<()> {
        let mut body = json!({ "access_code_id": access_code_id });
        if let Some(code) = new_code {
            body["code"] = json!(code);
        }
        if let Some((starts, ends)) = new_times {
            body["starts_at"] = json!(starts.to_rfc3339());
            body["ends_at"] = json!(ends.to_rfc3339());
        }

        let client = reqwest::Client::new();
        let response = client
            .post("https://connect.getseam.com/access_codes/update")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&body)
            .send()
            .await?;

        if response.status().is_success() {
            return Ok(());
        }

        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        Err(anyhow!(
            "Seam update failed for {} ({}): {}",
            access_code_id,
            status,
            text
        ))
    }

    /// Delete an access code from the device.
    ///
    /// As with update, Seam's reference says `DELETE` with query parameters while
    /// its example uses `POST` with a JSON body; `POST` matches the rest of this
    /// client, and a wrong method fails with a non-2xx response.
    pub async fn delete_access_code(&self, access_code_id: &str) -> Result<()> {
        let client = reqwest::Client::new();
        let response = client
            .post("https://connect.getseam.com/access_codes/delete")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&json!({ "access_code_id": access_code_id }))
            .send()
            .await?;

        if response.status().is_success() {
            return Ok(());
        }

        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        Err(anyhow!(
            "Seam delete failed for {} ({}): {}",
            access_code_id,
            status,
            text
        ))
    }

    /// Every access code on the device.
    pub async fn list_codes(&self) -> Result<Vec<ExistingCode>> {
        Ok(self
            .get_access_codes()
            .await?
            .iter()
            .filter_map(parse_existing_code)
            .collect())
    }

    /// Find the access code on the device that belongs to this event, if any.
    pub async fn find_existing_code(&self, event: &CalendarEvent) -> Result<Option<ExistingCode>> {
        let name = Self::code_name(event);
        let codes = self.get_access_codes().await?;
        Ok(codes
            .iter()
            .find(|c| c["name"].as_str() == Some(name))
            .and_then(parse_existing_code))
    }

    async fn get_access_codes(&self) -> Result<Vec<serde_json::Value>> {
        let client = reqwest::Client::new();
        let response = client
            .post("https://connect.getseam.com/access_codes/list")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&json!({ "device_id": &self.device_id }))
            .send()
            .await?;

        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(anyhow!("Failed to fetch access codes: {}", text));
        }

        let body: serde_json::Value = response.json().await?;
        Ok(body["access_codes"].as_array().cloned().unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};

    fn event(checkin: DateTime<Utc>, checkout: DateTime<Utc>) -> CalendarEvent {
        CalendarEvent {
            title: "Reserved".into(),
            description: String::new(),
            checkin,
            checkout,
            reservation_id: "gcal-1".into(),
            booking_id: Some("TESTRES0001".into()),
        }
    }

    fn base_times() -> (DateTime<Utc>, DateTime<Utc>) {
        (
            Utc.with_ymd_and_hms(2026, 11, 2, 23, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2026, 11, 5, 18, 0, 0).unwrap(),
        )
    }

    fn existing(code: Option<&str>, starts: DateTime<Utc>, ends: DateTime<Utc>) -> ExistingCode {
        ExistingCode {
            access_code_id: "ac-1".into(),
            name: Some("TESTRES0001".into()),
            code: code.map(str::to_string),
            starts_at: Some(starts),
            ends_at: Some(ends),
        }
    }

    #[test]
    fn no_existing_code_means_create() {
        let (ci, co) = base_times();
        assert_eq!(
            plan_code_action(None, &event(ci, co), "1234"),
            CodeAction::Create
        );
    }

    #[test]
    fn matching_code_and_times_is_unchanged() {
        let (ci, co) = base_times();
        let ex = existing(Some("1234"), ci, co);
        assert_eq!(
            plan_code_action(Some(&ex), &event(ci, co), "1234"),
            CodeAction::Unchanged
        );
    }

    #[test]
    fn sub_minute_jitter_is_unchanged() {
        let (ci, co) = base_times();
        let ex = existing(Some("1234"), ci + Duration::seconds(30), co);
        assert_eq!(
            plan_code_action(Some(&ex), &event(ci, co), "1234"),
            CodeAction::Unchanged
        );
    }

    #[test]
    fn modified_checkin_updates_times_only() {
        let (ci, co) = base_times();
        let ex = existing(Some("1234"), ci, co);
        let new_ci = ci + Duration::hours(2); // guest granted late checkin
        match plan_code_action(Some(&ex), &event(new_ci, co), "1234") {
            CodeAction::Update {
                access_code_id,
                new_code,
                new_times,
            } => {
                assert_eq!(access_code_id, "ac-1");
                assert_eq!(new_code, None);
                assert_eq!(new_times, Some((new_ci, co)));
            }
            other => panic!("expected Update, got {other:?}"),
        }
    }

    #[test]
    fn changed_pin_updates_code_only() {
        let (ci, co) = base_times();
        let ex = existing(Some("1234"), ci, co);
        match plan_code_action(Some(&ex), &event(ci, co), "9876") {
            CodeAction::Update {
                new_code,
                new_times,
                ..
            } => {
                assert_eq!(new_code.as_deref(), Some("9876"));
                assert_eq!(new_times, None);
            }
            other => panic!("expected Update, got {other:?}"),
        }
    }

    #[test]
    fn code_and_times_changed_together() {
        let (ci, co) = base_times();
        let ex = existing(Some("1234"), ci, co);
        let new_co = co + Duration::hours(3);
        match plan_code_action(Some(&ex), &event(ci, new_co), "9876") {
            CodeAction::Update {
                new_code,
                new_times,
                ..
            } => {
                assert_eq!(new_code.as_deref(), Some("9876"));
                assert_eq!(new_times, Some((ci, new_co)));
            }
            other => panic!("expected Update, got {other:?}"),
        }
    }

    #[test]
    fn unreported_code_is_not_treated_as_changed() {
        // If Seam omits the PIN, don't rewrite it on every run.
        let (ci, co) = base_times();
        let ex = existing(None, ci, co);
        assert_eq!(
            plan_code_action(Some(&ex), &event(ci, co), "1234"),
            CodeAction::Unchanged
        );
    }

    #[test]
    fn unreported_times_are_not_treated_as_changed() {
        let (ci, co) = base_times();
        let ex = ExistingCode {
            access_code_id: "ac-1".into(),
            name: Some("TESTRES0001".into()),
            code: Some("1234".into()),
            starts_at: None,
            ends_at: None,
        };
        let moved = ci + Duration::hours(2);
        assert_eq!(
            plan_code_action(Some(&ex), &event(moved, co), "1234"),
            CodeAction::Unchanged
        );
    }

    fn code(
        id: &str,
        name: Option<&str>,
        starts: Option<DateTime<Utc>>,
        ends: Option<DateTime<Utc>>,
    ) -> ExistingCode {
        ExistingCode {
            access_code_id: id.into(),
            name: name.map(str::to_string),
            code: Some("0000".into()),
            starts_at: starts,
            ends_at: ends,
        }
    }

    fn event_with_phone(ci: DateTime<Utc>, co: DateTime<Utc>) -> CalendarEvent {
        CalendarEvent {
            description: "Reservation: TESTRES0001\nPhone Number (Last 4 Digits): 2250".into(),
            ..event(ci, co)
        }
    }

    #[test]
    fn audit_classifies_events() {
        let (ci, co) = base_times();
        let now = ci - Duration::days(10);
        let events = vec![event_with_phone(ci, co)];

        // No code on the device: needs create.
        let none: Vec<ExistingCode> = vec![];
        let r = audit_codes(&events, &none, now);
        assert_eq!(r.needs_create.len(), 1);
        assert_eq!(r.issue_count(), 1);

        // Matching code: in sync.
        let ok = vec![ExistingCode {
            code: Some("2250".into()),
            ..code("a", Some("TESTRES0001"), Some(ci), Some(co))
        }];
        let r = audit_codes(&events, &ok, now);
        assert_eq!(r.in_sync.len(), 1);
        assert_eq!(r.issue_count(), 0);

        // Drifted window: needs update.
        let drift = vec![ExistingCode {
            code: Some("2250".into()),
            ..code(
                "a",
                Some("TESTRES0001"),
                Some(ci + Duration::hours(2)),
                Some(co),
            )
        }];
        let r = audit_codes(&events, &drift, now);
        assert_eq!(r.needs_update.len(), 1);
    }

    #[test]
    fn audit_event_without_phone_is_reported_not_created() {
        let (ci, co) = base_times();
        let events = vec![event(ci, co)];
        let none: Vec<ExistingCode> = vec![];
        let r = audit_codes(&events, &none, ci - Duration::days(1));
        assert_eq!(r.no_phone.len(), 1);
        assert_eq!(r.issue_count(), 0);
    }

    #[test]
    fn audit_flags_future_unmatched_code_as_orphan() {
        let (ci, co) = base_times();
        let now = ci - Duration::days(10);
        let codes = vec![code("x", Some("CANCELLED1"), Some(ci), Some(co))];
        let r = audit_codes(&[], &codes, now);
        assert_eq!(r.orphans.len(), 1);
        assert_eq!(r.issue_count(), 1);
    }

    #[test]
    fn audit_does_not_flag_mid_stay_expired_or_permanent_codes() {
        let (ci, co) = base_times();
        let now = ci + Duration::hours(5); // mid-stay
        let codes = vec![
            code("stay", Some("INHOUSE1"), Some(ci), Some(co)),
            code(
                "old",
                Some("PAST1"),
                Some(ci - Duration::days(9)),
                Some(ci - Duration::days(6)),
            ),
            code("owner", Some("Owner"), None, None),
            code("unnamed", None, Some(ci), Some(co)),
        ];
        let r = audit_codes(&[], &codes, now);
        assert!(r.orphans.is_empty());
        assert_eq!(r.in_progress.len(), 1);
        assert_eq!(r.unmanaged.len(), 2);
        assert_eq!(r.issue_count(), 0);
    }

    #[test]
    fn parses_seam_list_entry() {
        let v = json!({
            "access_code_id": "ac-1",
            "name": "TESTRES0001",
            "code": "1234",
            "starts_at": "2026-11-02T23:00:00.000Z",
            "ends_at": "2026-11-05T18:00:00.000Z"
        });
        let parsed = parse_existing_code(&v).unwrap();
        assert_eq!(parsed.access_code_id, "ac-1");
        assert_eq!(parsed.name.as_deref(), Some("TESTRES0001"));
        assert_eq!(parsed.code.as_deref(), Some("1234"));
        assert_eq!(parsed.starts_at, Some(base_times().0));
        assert_eq!(parsed.ends_at, Some(base_times().1));
    }

    #[test]
    fn entry_without_id_is_rejected() {
        assert!(parse_existing_code(&json!({ "code": "1234" })).is_none());
    }
}
