use chrono::{DateTime, NaiveDate, Utc};

/// A reservation parsed from an ICS feed
#[derive(Debug, Clone)]
pub struct IcsReservation {
    pub reservation_id: String,
    pub checkin_date: NaiveDate,
    pub checkout_date: NaiveDate,
    pub reservation_url: Option<String>,
    pub phone_last_4: Option<String>,
}

/// A timed calendar event (checkin/checkout, not all-day)
#[derive(Debug, Clone)]
pub struct CalendarEvent {
    pub title: String,
    pub description: String,
    pub checkin: DateTime<Utc>,
    pub checkout: DateTime<Utc>,
    /// Google Calendar event ID — fallback Seam code name
    pub reservation_id: String,
    /// Reservation ID extracted from description — preferred Seam code name
    pub booking_id: Option<String>,
}

/// Configuration loaded from environment
#[derive(Debug, Clone)]
pub struct Config {
    pub google_service_account_json: String,
    pub google_calendar_id: String,
    pub ics_url: String,
    pub log_file: String,
    pub log_level: String,
    pub checkin_time: String,
    pub checkout_time: String,
    pub seam_api_key: String,
    pub seam_device_id: String,
    pub gmail_client_id: String,
    pub gmail_client_secret: String,
    pub gmail_refresh_token: String,
}

impl Config {
    pub fn from_env() -> Result<Self, Box<dyn std::error::Error>> {
        dotenv::dotenv().ok();

        Ok(Config {
            google_service_account_json: std::env::var("GOOGLE_SERVICE_ACCOUNT_JSON")
                .unwrap_or_else(|_| "./service-account-key.json".to_string()),
            google_calendar_id: std::env::var("GOOGLE_CALENDAR_ID")?,
            ics_url: std::env::var("ICS_URL").or_else(|_| std::env::var("AIRBNB_ICS_URL"))?,
            log_file: std::env::var("LOG_FILE").unwrap_or_else(|_| "./innkeep.log".to_string()),
            log_level: std::env::var("LOG_LEVEL").unwrap_or_else(|_| "info".to_string()),
            checkin_time: std::env::var("CHECKIN_TIME").unwrap_or_else(|_| "15:00".to_string()),
            checkout_time: std::env::var("CHECKOUT_TIME").unwrap_or_else(|_| "10:00".to_string()),
            seam_api_key: std::env::var("SEAM_API_KEY").unwrap_or_default(),
            seam_device_id: std::env::var("SEAM_DEVICE_ID").unwrap_or_default(),
            gmail_client_id: std::env::var("GMAIL_CLIENT_ID").unwrap_or_default(),
            gmail_client_secret: std::env::var("GMAIL_CLIENT_SECRET").unwrap_or_default(),
            gmail_refresh_token: std::env::var("GMAIL_REFRESH_TOKEN").unwrap_or_default(),
        })
    }
}
