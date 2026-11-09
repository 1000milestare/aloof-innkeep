//! Mail the sync reads, from two sources that produce the same `Mail`:
//! - the KV store filled by the Cloudflare Email Worker (no Gmail needed)
//! - Gmail, while it is still configured
//!
//! Who may send what, and which value is newest, is decided here once.

use crate::gmail::{self, ReservationTimes, TimeCommand};
use crate::models::Config;
use anyhow::Result;
use std::collections::{BTreeSet, HashMap};

pub const AIRBNB_SENDER: &str = "automated@airbnb.com";

/// One email, reduced to what the sync needs.
#[derive(Debug, Clone)]
pub struct Mail {
    /// Bare lowercase address from the From header.
    pub from: String,
    pub subject: String,
    /// Milliseconds since the epoch.
    pub received_ms: i64,
    /// Plain-text body.
    pub text: String,
}

/// "Name <a@b.com>" or "a@b.com" → "a@b.com", lowercased.
pub fn bare_address(value: &str) -> String {
    let inner = match (value.find('<'), value.find('>')) {
        (Some(a), Some(b)) if a < b => &value[a + 1..b],
        _ => value,
    };
    inner.trim().to_lowercase()
}

/// First text/plain part of a MIME tree.
fn plain_text(part: &mailparse::ParsedMail) -> Option<String> {
    if part.subparts.is_empty() {
        if part.ctype.mimetype.eq_ignore_ascii_case("text/plain") {
            return part.get_body().ok();
        }
        return None;
    }
    part.subparts.iter().find_map(plain_text)
}

/// Parse a raw RFC 5322 message as stored by the Worker.
pub fn parse_raw(raw: &str, received_ms: i64) -> Option<Mail> {
    use mailparse::MailHeaderMap;
    let parsed = mailparse::parse_mail(raw.as_bytes()).ok()?;
    let header = |name: &str| parsed.headers.get_first_value(name).unwrap_or_default();
    Some(Mail {
        from: bare_address(&header("From")),
        subject: header("Subject"),
        received_ms,
        text: plain_text(&parsed).unwrap_or_default(),
    })
}

/// Cloudflare KV location of the stored mail (read via the REST API).
#[derive(Debug, Clone)]
pub struct KvConfig {
    pub account_id: String,
    pub namespace_id: String,
    pub api_token: String,
}

impl KvConfig {
    /// None unless all three settings are present.
    pub fn from_config(config: &Config) -> Option<Self> {
        if config.cf_account_id.is_empty()
            || config.cf_kv_namespace_id.is_empty()
            || config.cf_api_token.is_empty()
        {
            return None;
        }
        Some(Self {
            account_id: config.cf_account_id.clone(),
            namespace_id: config.cf_kv_namespace_id.clone(),
            api_token: config.cf_api_token.clone(),
        })
    }
}

/// Milliseconds from a key of the form "mail:<ms>:<uuid>".
fn key_timestamp(key: &str) -> Option<i64> {
    key.split(':').nth(1)?.parse().ok()
}

/// Every mail the Worker has stored that is still within retention.
pub async fn fetch_kv_mail(cfg: &KvConfig) -> Result<Vec<Mail>> {
    let client = reqwest::Client::new();
    let base = format!(
        "https://api.cloudflare.com/client/v4/accounts/{}/storage/kv/namespaces/{}",
        cfg.account_id, cfg.namespace_id
    );

    let mut keys: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let mut url = format!("{}/keys?prefix=mail:&limit=1000", base);
        if let Some(c) = &cursor {
            url.push_str(&format!("&cursor={}", urlencoding::encode(c)));
        }
        let body: serde_json::Value = client
            .get(&url)
            .bearer_auth(&cfg.api_token)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        if let Some(items) = body["result"].as_array() {
            keys.extend(
                items
                    .iter()
                    .filter_map(|k| k["name"].as_str().map(String::from)),
            );
        }
        cursor = body["result_info"]["cursor"]
            .as_str()
            .filter(|c| !c.is_empty())
            .map(String::from);
        if cursor.is_none() {
            break;
        }
    }

    let mut mails = Vec::new();
    for key in keys {
        let url = format!("{}/values/{}", base, urlencoding::encode(&key));
        let resp = client.get(&url).bearer_auth(&cfg.api_token).send().await?;
        if !resp.status().is_success() {
            tracing::warn!("Could not read stored mail {}", key);
            continue;
        }
        let raw = resp.text().await?;
        match parse_raw(&raw, key_timestamp(&key).unwrap_or(0)) {
            Some(mail) => mails.push(mail),
            None => tracing::warn!("Could not parse stored mail {}", key),
        }
    }
    Ok(mails)
}

/// Stored mail from Cloudflare KV, read once per run. Empty if KV isn't configured.
pub async fn load_kv(config: &Config) -> Result<Vec<Mail>> {
    let Some(kv) = KvConfig::from_config(config) else {
        return Ok(Vec::new());
    };
    let stored = fetch_kv_mail(&kv).await?;
    tracing::info!(
        "Read {} stored mail message(s) from Cloudflare KV",
        stored.len()
    );
    Ok(stored)
}

/// Mail from every configured source for these reservations. `kv` is mail already
/// read by `load_kv`, so a run reads KV only once.
pub async fn collect_with(
    config: &Config,
    reservation_ids: &[String],
    kv: &[Mail],
) -> Result<Vec<Mail>> {
    let mut mails = kv.to_vec();
    if gmail_configured(config) {
        mails.extend(
            gmail::fetch_mail_for_reservations(
                &config.gmail_client_id,
                &config.gmail_client_secret,
                &config.gmail_refresh_token,
                &config.command_sender,
                reservation_ids,
            )
            .await?,
        );
    }
    Ok(mails)
}

/// Reservation codes named by cancellation emails from every configured source.
/// `kv` is mail already read by `load_kv`.
pub async fn cancelled_ids_with(config: &Config, kv: &[Mail]) -> Result<BTreeSet<String>> {
    let mut ids = cancelled_codes(kv);
    if gmail_configured(config) {
        ids.extend(
            gmail::fetch_cancelled_reservation_ids(
                &config.gmail_client_id,
                &config.gmail_client_secret,
                &config.gmail_refresh_token,
                crate::commands::cancel::CANCELLATION_LOOKBACK_DAYS,
            )
            .await?,
        );
    }
    Ok(ids)
}

/// Reservation codes named by cancellation emails, reading KV itself.
pub async fn cancelled_ids(config: &Config) -> Result<BTreeSet<String>> {
    let kv = load_kv(config).await?;
    cancelled_ids_with(config, &kv).await
}

pub fn gmail_configured(config: &Config) -> bool {
    !config.gmail_client_id.is_empty()
        && !config.gmail_client_secret.is_empty()
        && !config.gmail_refresh_token.is_empty()
}

/// Reservation codes named by Airbnb cancellation subjects in these mails.
pub fn cancelled_codes(mails: &[Mail]) -> BTreeSet<String> {
    mails
        .iter()
        .filter(|m| m.from == AIRBNB_SENDER)
        .filter_map(|m| gmail::parse_cancellation_subject(&m.subject))
        .collect()
}

/// Check-in and checkout per reservation. Airbnb reminders come from Airbnb;
/// commands come from the owner. A reservation with neither is absent from the
/// result, so the caller uses the config defaults.
pub fn reservation_times(
    mails: &[Mail],
    reservation_ids: &[String],
    command_sender: &str,
    default_checkin: &str,
    default_checkout: &str,
) -> HashMap<String, ReservationTimes> {
    let owners: Vec<String> = command_sender
        .split(',')
        .map(bare_address)
        .filter(|s| !s.is_empty())
        .collect();
    let mut out = HashMap::new();

    for rid in reservation_ids {
        let mut reminders = Vec::new();
        let mut commands: Vec<(i64, TimeCommand)> = Vec::new();

        for mail in mails {
            if mail.from == AIRBNB_SENDER {
                if let Some(times) = gmail::parse_times_from_email(&mail.text, rid) {
                    reminders.push((mail.received_ms, times));
                }
            } else if owners.contains(&mail.from) {
                // Only the first check-in and first check-out in one message count, so
                // a later line (for example one quoted from an earlier message) can't
                // override the newest instruction at the top.
                let mut found = gmail::parse_time_commands(&mail.text, rid);
                let (mut seen_in, mut seen_out) = (false, false);
                found.retain(|c| match c {
                    gmail::TimeCommand::CheckIn(_) => !std::mem::replace(&mut seen_in, true),
                    gmail::TimeCommand::CheckOut(_) => !std::mem::replace(&mut seen_out, true),
                });
                for c in &found {
                    tracing::info!(
                        "{}: command {:?} received at {} ms",
                        rid,
                        c,
                        mail.received_ms
                    );
                }
                commands.extend(found.into_iter().map(|c| (mail.received_ms, c)));
            } else if !gmail::parse_time_commands(&mail.text, rid).is_empty() {
                tracing::warn!(
                    "{}: command ignored, sender not in COMMAND_SENDER ({} configured)",
                    rid,
                    owners.len()
                );
            }
        }

        if let Some(times) =
            gmail::resolve_times(reminders, commands, default_checkin, default_checkout)
        {
            out.insert(rid.clone(), times);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const RAW: &str = "From: Owner <owner@gmail.com>\r\nSubject: Fwd: change\r\nContent-Type: text/plain; charset=utf-8\r\n\r\ncheckout HMTEST12345 13:00\r\n";

    #[test]
    fn bare_address_handles_display_names() {
        assert_eq!(
            bare_address("\"Owner\" <Owner@Gmail.com>"),
            "owner@gmail.com"
        );
        assert_eq!(bare_address("automated@airbnb.com"), "automated@airbnb.com");
    }

    #[test]
    fn parses_stored_raw_mail() {
        let mail = parse_raw(RAW, 42).unwrap();
        assert_eq!(mail.from, "owner@gmail.com");
        assert_eq!(mail.subject, "Fwd: change");
        assert_eq!(mail.received_ms, 42);
        assert!(mail.text.contains("checkout HMTEST12345 13:00"));
    }

    #[test]
    fn picks_text_part_of_multipart_mail() {
        let raw = "From: automated@airbnb.com\r\nSubject: Canceled: Reservation HMTEST12345 for Sep 1\r\nContent-Type: multipart/alternative; boundary=\"b\"\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nplain body\r\n--b\r\nContent-Type: text/html\r\n\r\n<p>html</p>\r\n--b--\r\n";
        let mail = parse_raw(raw, 1).unwrap();
        assert_eq!(mail.text.trim(), "plain body");
    }

    #[test]
    fn key_timestamp_reads_the_millisecond_prefix() {
        assert_eq!(key_timestamp("mail:1789600000000:abc"), Some(1789600000000));
        assert_eq!(key_timestamp("other"), None);
    }

    #[test]
    fn times_come_from_owner_and_airbnb_only() {
        let mails = vec![
            Mail {
                from: "owner@gmail.com".into(),
                subject: String::new(),
                received_ms: 200,
                text: "checkout HMTEST12345 13:00".into(),
            },
            // A guest-style line from another sender must not change times.
            Mail {
                from: "guest@example.com".into(),
                subject: String::new(),
                received_ms: 300,
                text: "checkout HMTEST12345 09:00".into(),
            },
        ];
        let rids = vec!["HMTEST12345".to_string()];
        let out = reservation_times(&mails, &rids, "owner@gmail.com", "15:00", "10:00");
        assert_eq!(out["HMTEST12345"].checkout_time, "13:00");
        assert_eq!(out["HMTEST12345"].checkin_time, "15:00");
    }

    #[test]
    fn first_checkout_in_newest_message_wins() {
        // Same message holds a new instruction on top and an older one below it.
        let mails = vec![Mail {
            from: "owner@gmail.com".into(),
            subject: String::new(),
            received_ms: 500,
            text: "checkout HMTEST12345 13:00\n\nOn Sat, you wrote:\ncheckout HMTEST12345 01:00\n"
                .into(),
        }];
        let rids = vec!["HMTEST12345".to_string()];
        let out = reservation_times(&mails, &rids, "owner@gmail.com", "15:00", "10:00");
        assert_eq!(out["HMTEST12345"].checkout_time, "13:00");
    }

    #[test]
    fn cancellations_only_from_airbnb() {
        let mails = vec![
            Mail {
                from: AIRBNB_SENDER.into(),
                subject: "Canceled: Reservation HMTEST12345 for Sep 1".into(),
                received_ms: 1,
                text: String::new(),
            },
            Mail {
                from: "owner@gmail.com".into(),
                subject: "Canceled: Reservation HMTEST99999 for Sep 1".into(),
                received_ms: 1,
                text: String::new(),
            },
        ];
        let ids: Vec<String> = cancelled_codes(&mails).into_iter().collect();
        assert_eq!(ids, vec!["HMTEST12345".to_string()]);
    }
}
