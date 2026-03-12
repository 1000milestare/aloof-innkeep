# aloof-innkeep

Syncs a vacation rental ICS calendar to Google Calendar and creates Seam smart lock codes for each booking.

## How it works

1. **sync** — Fetches the ICS feed, parses reservations, creates timed Google Calendar events (all-day → check-in at 3 PM, checkout at 10 AM)
2. **create-codes** — Reads Google Calendar, finds bookable future events, extracts the guest's phone last-4, creates a Seam access code

Both phases run by default. Either can be run alone.

## Setup

**Prerequisites:** Rust 1.75+, a Google Cloud service account with Calendar API access, optionally a Seam workspace.

### Google Calendar

1. [Google Cloud Console](https://console.cloud.google.com/) → enable Calendar API → create a service account → download JSON key as `service-account-key.json`
2. Share your Google Calendar with the service account email (`Make changes to events`)

### Environment

Copy `.env.example` to `.env` and fill in:

```
GOOGLE_SERVICE_ACCOUNT_JSON=./service-account-key.json
GOOGLE_CALENDAR_ID=your-calendar-id@group.calendar.google.com
ICS_URL=https://www.airbnb.com/calendar/ical/...
CHECKIN_TIME=15:00
CHECKOUT_TIME=10:00
LOG_FILE=./innkeep.log
LOG_LEVEL=info

# Optional: Seam smart lock
SEAM_API_KEY=your-workspace-api-key
SEAM_DEVICE_ID=your-device-id
```

> `AIRBNB_ICS_URL` is also accepted as a fallback for `ICS_URL`.

## Usage

```bash
cargo build --release

./target/release/aloof-innkeep              # full run
./target/release/aloof-innkeep sync         # ICS → Google only
./target/release/aloof-innkeep create-codes # Google → Seam only
./target/release/aloof-innkeep --dry-run    # preview any of the above
```

### Cron

```cron
0 */4 * * * /path/to/aloof-innkeep >> /tmp/innkeep.log 2>&1
```

## Project structure

```
src/
├── main.rs               # Command dispatcher
├── models.rs             # Structs and config
├── ics.rs                # ICS fetch and parse
├── google.rs             # Google Calendar API
├── seam.rs               # Seam smart lock API
├── event_filter.rs       # Phone extraction, event filtering
├── logger.rs             # Logging setup
└── commands/
    ├── sync.rs           # ICS → Google
    └── create_codes.rs   # Google → Seam
```
