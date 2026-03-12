use crate::models::CalendarEvent;
use anyhow::{anyhow, Result};
use serde_json::json;

pub struct SeamClient {
    api_key: String,
    device_id: String,
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

    pub async fn code_exists(&self, event: &CalendarEvent) -> Result<bool> {
        let name = Self::code_name(event);
        let codes = self.get_access_codes().await?;
        Ok(codes.iter().any(|c| c["name"].as_str() == Some(name)))
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
