use lazy_static::lazy_static;
use regex::Regex;

lazy_static! {
    // HTML entities
    static ref HTML_ENTITY_REGEX: Regex = Regex::new(r"&(?:amp|lt|gt|nbsp|quot|apos);").unwrap();
    // ICS escaped semicolons: \;
    static ref ICS_ESCAPED_SEMI: Regex = Regex::new(r"\\;").unwrap();
    // <br> variants
    static ref HTML_BR_REGEX: Regex = Regex::new(r"(?i)<br\s*/?>").unwrap();
    // All remaining HTML tags
    static ref HTML_TAG_REGEX: Regex = Regex::new(r"<[^>]+>").unwrap();

    // Airbnb reservation ID in URL: /details/HMXXXXXXXX or itinerary?code=HMXXXXXXXX
    static ref AIRBNB_ID_REGEX: Regex = Regex::new(r"(?:details|code)[=/]([A-Z0-9]{8,})").unwrap();

    // Airbnb native format (from their ICS/API): "Phone Number (Last 4 Digits): 2250"
    static ref PHONE_AIRBNB_REGEX: Regex = Regex::new(r"Phone\s+Number\s*\(.*?\):\s*(\d{4})").unwrap();
    // Our own sync-created format: "Phone (Last 4): 4500"
    static ref PHONE_OWN_REGEX: Regex = Regex::new(r"Phone\s*\(Last\s*4\):\s*(\d{4})").unwrap();
    // Generic format for manually created events: "Phone: 5839" or "phone: 5839"
    // Requires colon immediately after "Phone" to avoid false-matching "Phone (Last 4):" above
    static ref PHONE_GENERIC_REGEX: Regex = Regex::new(r"[Pp]hone:\s*(\d{4})").unwrap();
    // Title format: "- 5839" or "-5839" (ends with dash + optional space + 4 digits)
    static ref PHONE_TITLE_REGEX: Regex = Regex::new(r"-\s*(\d{4})$").unwrap();
}

/// Strip HTML tags and decode common entities so regexes work on plain text.
pub fn strip_html(input: &str) -> String {
    let s = ICS_ESCAPED_SEMI.replace_all(input, ";");
    let s = HTML_BR_REGEX.replace_all(&s, "\n");
    let s = HTML_TAG_REGEX.replace_all(&s, "");
    let s = HTML_ENTITY_REGEX.replace_all(&s, |caps: &regex::Captures| match &caps[0] {
        "&amp;" => "&".to_string(),
        "&lt;" => "<".to_string(),
        "&gt;" => ">".to_string(),
        "&nbsp;" => " ".to_string(),
        "&quot;" => "\"".to_string(),
        "&apos;" => "'".to_string(),
        other => other.to_string(),
    });
    s.to_string()
}

/// Extract phone using Airbnb format: "Phone Number (Last 4 Digits): XXXX"
pub fn extract_phone_airbnb_format(description: &str) -> Option<String> {
    let plain = strip_html(description);
    PHONE_AIRBNB_REGEX
        .captures(&plain)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string())
}

/// Extract phone using our own sync-created format: "Phone (Last 4): XXXX"
pub fn extract_phone_own_format(description: &str) -> Option<String> {
    let plain = strip_html(description);
    PHONE_OWN_REGEX
        .captures(&plain)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string())
}

/// Extract phone using generic format: "Phone: XXXX" or "phone: XXXX"
pub fn extract_phone_generic_format(description: &str) -> Option<String> {
    let plain = strip_html(description);
    PHONE_GENERIC_REGEX
        .captures(&plain)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string())
}

/// Extract 4-digit phone number from event title: "-5839" at end
pub fn extract_phone_from_title(title: &str) -> Option<String> {
    PHONE_TITLE_REGEX
        .captures(title)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string())
}

/// Extract phone from multiple sources in priority order:
/// 1. Description: "Phone Number (Last 4 Digits): XXXX" (Airbnb native ICS format)
/// 2. Description: "Phone (Last 4): XXXX"              (our sync-created format)
/// 3. Description: "Phone: XXXX"                       (manually created events)
/// 4. Title: "-XXXX"                                   (title suffix)
pub fn extract_phone(description: &str, title: &str) -> Option<String> {
    extract_phone_airbnb_format(description)
        .or_else(|| extract_phone_own_format(description))
        .or_else(|| extract_phone_generic_format(description))
        .or_else(|| extract_phone_from_title(title))
}

/// Check if an event is bookable (has phone number)
#[allow(dead_code)]
pub fn is_bookable_event(description: &str, title: &str) -> bool {
    extract_phone(description, title).is_some()
}

/// Extract Airbnb reservation ID from description.
/// Searches the raw text (before HTML stripping) since the URL may be in an href attribute.
/// Handles both URL formats:
///   - /hosting/reservations/details/HMXXXXXXXX
///   - /reservation/itinerary?code=HMXXXXXXXX
pub fn extract_airbnb_id(description: &str) -> Option<String> {
    AIRBNB_ID_REGEX
        .captures(description)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ============ HTML stripping ============

    #[test]
    fn test_strip_html_br_to_newline() {
        assert!(strip_html("foo<br>bar").contains('\n'));
        assert!(strip_html("foo<br/>bar").contains('\n'));
        assert!(strip_html("foo<BR>bar").contains('\n'));
    }

    #[test]
    fn test_strip_html_removes_anchor() {
        let html = r#"<a href="https://airbnb.com">link</a>"#;
        assert_eq!(strip_html(html), "link");
    }

    #[test]
    fn test_strip_html_decodes_amp() {
        assert_eq!(strip_html("a &amp; b"), "a & b");
    }

    #[test]
    fn test_strip_html_ics_escaped_semi() {
        assert_eq!(strip_html(r"a\;b"), "a;b");
    }

    // ============ Airbnb format (HTML-wrapped) ============

    #[test]
    fn test_extract_phone_airbnb_html() {
        let desc = r#"Reservation URL: <a href="https://none">none</a><br>Phone Number (Last 4 Digits): 2250<br>Notes: test"#;
        assert_eq!(extract_phone_airbnb_format(desc), Some("2250".to_string()));
    }

    #[test]
    fn test_extract_phone_airbnb_plain() {
        let desc = "Guest: Andy\nPhone Number (Last 4 Digits): 2250";
        assert_eq!(extract_phone_airbnb_format(desc), Some("2250".to_string()));
    }

    // ============ Generic format ============

    #[test]
    fn test_extract_phone_generic_colon() {
        assert_eq!(
            extract_phone_generic_format("Phone: 5839"),
            Some("5839".to_string())
        );
    }

    #[test]
    fn test_extract_phone_generic_lowercase() {
        assert_eq!(
            extract_phone_generic_format("phone: 1234"),
            Some("1234".to_string())
        );
    }

    #[test]
    fn test_extract_phone_generic_multiline() {
        assert_eq!(
            extract_phone_generic_format("Guest: John\nPhone: 5839\nNotes: extra"),
            Some("5839".to_string())
        );
    }

    #[test]
    fn test_extract_phone_generic_no_match_own_format() {
        // "Phone (Last 4): XXXX" should NOT match generic — handled by own-format extractor
        assert_eq!(extract_phone_generic_format("Phone (Last 4): 4500"), None);
    }

    #[test]
    fn test_extract_phone_generic_no_phone() {
        assert_eq!(extract_phone_generic_format("Just a note"), None);
    }

    // ============ Own sync-created format ============

    #[test]
    fn test_extract_phone_own_format_basic() {
        assert_eq!(
            extract_phone_own_format("Phone (Last 4): 4500"),
            Some("4500".to_string())
        );
    }

    #[test]
    fn test_extract_phone_own_format_in_context() {
        let desc = "Airbnb Reservation: HM5N9MFZ8J\n\nReservation URL: https://www.airbnb.com/hosting/reservations/details/HM5N9MFZ8J\nPhone (Last 4): 4500";
        assert_eq!(extract_phone_own_format(desc), Some("4500".to_string()));
    }

    #[test]
    fn test_extract_phone_own_format_not_matched_by_airbnb() {
        // Should fall through airbnb format and be caught by own format
        let desc = "Airbnb Reservation: HM2W4NC5JZ\n\nReservation URL: https://www.airbnb.com/hosting/reservations/details/HM2W4NC5JZ\nPhone (Last 4): 6701";
        assert_eq!(
            extract_phone(desc, "Reserved: HM2W4NC5JZ"),
            Some("6701".to_string())
        );
    }

    // ============ Title format ============

    #[test]
    fn test_extract_phone_from_title_with_dash() {
        assert_eq!(
            extract_phone_from_title("Manual Booking - 5839"),
            Some("5839".to_string())
        );
    }

    #[test]
    fn test_extract_phone_from_title_no_phone() {
        assert_eq!(extract_phone_from_title("Regular Event"), None);
    }

    #[test]
    fn test_extract_phone_from_title_too_short() {
        assert_eq!(extract_phone_from_title("Event -123"), None);
    }

    #[test]
    fn test_extract_phone_from_title_too_long() {
        assert_eq!(extract_phone_from_title("Event -12345"), None);
    }

    // ============ Combined / priority ============

    #[test]
    fn test_extract_phone_airbnb_beats_generic() {
        let desc = "Phone Number (Last 4 Digits): 1111\nPhone: 2222";
        assert_eq!(extract_phone(desc, ""), Some("1111".to_string()));
    }

    #[test]
    fn test_extract_phone_description_beats_title() {
        assert_eq!(
            extract_phone("Phone: 1111", "Event - 2222"),
            Some("1111".to_string())
        );
    }

    #[test]
    fn test_extract_phone_fallback_to_title() {
        assert_eq!(
            extract_phone("No phone here", "Booking - 5839"),
            Some("5839".to_string())
        );
    }

    #[test]
    fn test_extract_phone_neither_found() {
        assert_eq!(extract_phone("Description", "Title"), None);
    }

    #[test]
    fn test_extract_phone_empty() {
        assert_eq!(extract_phone("", ""), None);
    }

    #[test]
    fn test_extract_phone_complex_html_description() {
        let desc = r#"Reservation URL: <a href="https://none">n</a>one<br>Phone Number (Last 4 Digits): 2250<br>Notes: andy's wedding guests stay"#;
        assert_eq!(
            extract_phone(desc, "Andy's wedding guests stay"),
            Some("2250".to_string())
        );
    }

    #[test]
    fn test_extract_phone_airbnb_synced_event() {
        let desc = r#"Reservation URL: <a href="https://www.google.com/url?q=https://www.airbnb.com/hosting/reservations/details/HMQ5T845CQ&amp;sa=D">https://www.airbnb.com/hosting/reservations/details/HMQ5T845CQ</a>
Phone Number (Last 4 Digits): 0447"#;
        assert_eq!(extract_phone(desc, "Reserved"), Some("0447".to_string()));
    }

    // ============ extract_airbnb_id ============

    #[test]
    fn test_extract_airbnb_id_details_url() {
        let desc = r#"Reservation URL: <a href="https://www.airbnb.com/hosting/reservations/details/HMQ5T845CQ">link</a>"#;
        assert_eq!(extract_airbnb_id(desc), Some("HMQ5T845CQ".to_string()));
    }

    #[test]
    fn test_extract_airbnb_id_itinerary_url() {
        let desc = "Reservation URL: https://www.airbnb.com/reservation/itinerary?code=HMFN8KMXYE\nPhone Number (Last 4 Digits): 0472";
        assert_eq!(extract_airbnb_id(desc), Some("HMFN8KMXYE".to_string()));
    }

    #[test]
    fn test_extract_airbnb_id_google_redirect() {
        // URL wrapped in Google redirect with \; escapes
        let desc = r#"Reservation URL: <a href="https://www.google.com/url?q=https://www.airbnb.com/hosting/reservations/details/HM883WWNPM&amp\;sa=D">link</a>\nPhone Number (Last 4 Digits): 8006"#;
        assert_eq!(extract_airbnb_id(desc), Some("HM883WWNPM".to_string()));
    }

    #[test]
    fn test_extract_airbnb_id_none() {
        assert_eq!(extract_airbnb_id("No URL here"), None);
    }

    // ============ is_bookable_event ============

    #[test]
    fn test_is_bookable_with_phone_in_description() {
        assert!(is_bookable_event("Phone: 5839", "Event"));
    }

    #[test]
    fn test_is_bookable_with_phone_in_title() {
        assert!(is_bookable_event("Description", "Booking - 5839"));
    }

    #[test]
    fn test_is_bookable_without_phone() {
        assert!(!is_bookable_event("Description", "Title"));
    }
}
