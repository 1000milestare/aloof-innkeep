# Mail Worker (Cloudflare Email Worker)

A minimal relay. For mail from the owner or from Airbnb's automated sender, it
stores the raw message in Cloudflare KV (kept 120 days), forwards a copy for humans,
and starts the GitHub Actions sync. It does not parse mail. The Rust
sync reads the stored mail and applies the same rules as before.

Each accepted message is also forwarded to `FORWARD_TO` (a human copy, a secret), so the Worker
must stay the routing action for the command address. Every message is forwarded to `FORWARD_TO`, whatever the sender. Only mail from a sender in `OWNER_SENDERS` (plus Airbnb's automated sender) is stored and acted on, and only when its DKIM signature is aligned with the From domain.

## Setup

1. **KV namespace:** `npx wrangler kv namespace create MAIL`. Keep the returned id out of git: the setup script writes it to `wrangler.local.toml`, and the deploy workflow reads it from the `CF_KV_NAMESPACE_ID` variable.
2. **Routing:** in Email Routing, point the command address at this Worker (replacing its
   forward rule; the Worker now does the forward). Leave the catch-all as it is.
3. **Secrets:** `OWNER_SENDERS` (comma-separated), `FORWARD_TO`, and `GITHUB_TOKEN` via `wrangler secret put`, or run the setup script. Use a fine-grained token with
   Actions: write on `1000milestare/aloof-innkeep`.
4. **Deploy:** push to `main` (the `worker.yml` workflow deploys), or run `npx wrangler deploy`.
   Deploys need the repo secret `CF_API_TOKEN` and the variable `CF_ACCOUNT_ID`.
5. **Sync side:** the workflow reads the same KV namespace. Set the repo variables
   `CF_ACCOUNT_ID` and `CF_KV_NAMESPACE_ID`, and the secret `CF_API_TOKEN` (needs KV read).

## Command format

From the owner's address, one change per line:

    checkout HMEXAMPLE01 13:00
    checkin HMEXAMPLE01 3:00 PM

## Checks

- `npm test` covers the sender and DKIM checks.
- Only the first `Authentication-Results` header is trusted.
