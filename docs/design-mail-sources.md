# Design: mail sources and neutral naming

Status: proposal for review. No behaviour changes until approved.

## Goals

1. One explicit setting says which mail sources are active. Presence of credentials never switches a source on.
2. Every source turns its mail into the same `Mail` record, so the rest of the code never knows where mail came from.
3. A failing source is reported, but never discards another source's results.
4. Public code and docs use generic terms. Booking-platform names appear only where a real format must be matched, and then only in the parser.
5. Owner commands are trusted from one list, which the Worker and the sync both read.

## Current state

- Mail comes from two places, Cloudflare KV and Gmail. Gmail is active whenever its three secrets are present.
- Sender trust is split: the Worker checks `OWNER_SENDERS` and the sync checks `COMMAND_SENDER`. They can drift, and last week they did.
- Each source is called separately from `mailbox.rs`, with its own error handling.

## Proposed design

### 1. Explicit source selection

`MAIL_SOURCES` is a comma-separated list. It is the only thing that decides which sources run.

- Default when unset: `cloudflare`.
- Allowed values: `cloudflare`, `gmail`.
- Unknown value: startup error.
- A listed source with missing credentials: startup error. No silent skip.

### 2. One mail record

All sources return the existing `Mail` record: `from`, `subject`, `received_ms`, `text`. Both sources are implemented behind one `MailSource` enum with an async `fetch` method. Gmail returns full messages, not IDs, so cancellations are derived from `Mail` in one place (`cancelled_codes`), the same way for every source.

### 3. Isolation

Each source is fetched separately. A source that fails is recorded as a failure and contributes nothing, and the other sources' results are still used. This is the behaviour in PR #36, moved into the connector layer.

### 4. One owner list

`OWNER_SENDERS` replaces `COMMAND_SENDER`. The setup script sets it once, and both the Worker secret and the org secret are written from that one input. The sync also checks that the list isn't empty when the Cloudflare source is active.

### 5. Platform sender

`PLATFORM_SENDER` (default: unset) is the address that booking reminders and cancellations come from. It replaces the hard-coded address. The Worker and the sync both read it, from the same setup input.

## Neutral naming

Names are changed where they're internal. Anything that must match a real format is kept in the parser, with a comment that names the format.

| Current | Proposed | Notes |
|---|---|---|
| `AIRBNB_SENDER` (const) | `PLATFORM_SENDER` (config) | Read from config, not a constant |
| `AIRBNB_ICS_URL` (env fallback) | `ICS_URL` only | Remove fallback after instance configs are updated |
| `COMMAND_SENDER` | `OWNER_SENDERS` | One list, shared with the Worker |
| `airbnb_id` / `booking_id` | `booking_id` | Internal identifier |
| `AIRBNB_ID_REGEX` | `BOOKING_ID_REGEX` | Keep the pattern, which matches the reservation URL |
| `extract_phone_airbnb_format` | `extract_phone_platform_format` | Keep the pattern |
| Worker `AIRBNB_SENDER` var | `PLATFORM_SENDER` | Same value as the sync |
| Log and comment text saying "Airbnb" | "booking platform" or "reservation" | |
| `seam-setup.sh` wording | "each reservation" | |
| README and examples | placeholder domain and ID | |

Kept, because they match a real format or are stored data:

- The reservation-URL pattern and the phone-digits line in the parser, with a comment naming the format.
- The `@airbnb-sync.local` calendar UID suffix. See the next section.

## Risk: persisted calendar UIDs

Each booking's calendar event and cleaning buffer have a UID ending `@airbnb-sync.local` (buffer: `-buffer@airbnb-sync.local`). The sync looks up existing events by UID. Changing the suffix would make the sync miss every existing event and create duplicates.

Two options:

- **A. Keep the suffix.** Simplest and safest. It's the only place the name still appears in the code, and it's a private identifier, not a public one. Recommended.
- **B. Rename with a migration.** Find each event by its reservation ID, not its UID, rewrite the UID, and do it once per booking. This needs its own review and a dry run.

Recommendation: A, and note the suffix in the code as a fixed internal identifier.

## Migration plan

Each step is reviewed and verified before the next one.

1. **This doc.** Review and approve.
2. **Gmail off.** Remove the three `GMAIL_*` org secrets. Run Scheduled Sync manually and confirm it's green, with overrides from Cloudflare only.
3. **Explicit `MAIL_SOURCES`.** Set `MAIL_SOURCES=cloudflare`. Add startup validation. Test that an unknown or unconfigured source fails at startup.
4. **Connector layer.** Move Gmail behind `MailSource`. Add a test that a failing source leaves the other's results in place.
5. **Owner list.** Replace `COMMAND_SENDER` with `OWNER_SENDERS`, set from one input in the setup script. Add a check that Worker and org secrets match.
6. **Naming.** Rename internal identifiers, config and docs. Make `PLATFORM_SENDER` config-driven in the sync and the Worker. Keep the UID suffix (option A).
7. **Remove Gmail.** Delete the Gmail code and `auth-gmail` after a few days of clean runs.

## Verification

- Unit tests for: source selection (valid, unknown, unconfigured), `Mail` derivation for each source, failure isolation, owner-list matching, and the reservation-URL and phone parsers.
- Worker tests for sender checks, using `PLATFORM_SENDER` and `OWNER_SENDERS`.
- After each step: one manual Scheduled Sync, checking the mail count, overrides, cancellations, and a green run.
- Before step 7: at least one new reservation and one time change handled end to end.

## Open questions

1. Is option A (keep the UID suffix) acceptable?
2. Should `ICS_URL` be the only feed variable, or keep `AIRBNB_ICS_URL` as a deprecated alias for a while?
3. Should cancellation-only mail from `PLATFORM_SENDER` be trusted without a DKIM check? Current behaviour requires it, and I'd keep it.
