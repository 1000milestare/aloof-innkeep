# Mail Worker (Cloudflare Email Worker)

A minimal relay. For mail from the owner or from Airbnb's automated sender, it
stores the raw message in Cloudflare KV (kept 120 days), forwards a copy for humans,
and starts the GitHub Actions sync. It does not parse mail. The Rust
sync reads the stored mail and applies the same rules as before.

Each accepted message is also forwarded to `FORWARD_TO` (a human copy), so the Worker
must stay the routing action for `beauty@prytzhouse.com`. Mail from anyone else is rejected. Guest messages (`express@airbnb.com`) are never stored.

## Setup

1. **KV namespace:** `npx wrangler kv namespace create MAIL`. Put the returned id in `wrangler.toml`.
2. **Routing:** in Email Routing, point `beauty@prytzhouse.com` at this Worker (replacing its
   forward rule; the Worker now does the forward). Leave the catch-all as it is.
3. **Secrets:** `npx wrangler secret put GITHUB_TOKEN`. Use a fine-grained token with
   Actions: write on `1000milestare/aloof-innkeep`.
4. **Deploy:** push to `main` (the `worker.yml` workflow deploys), or run `npx wrangler deploy`.
   Deploys need the repo secret `CF_API_TOKEN` and the variable `CF_ACCOUNT_ID`.
5. **Sync side:** the workflow reads the same KV namespace. Set the repo variables
   `CF_ACCOUNT_ID` and `CF_KV_NAMESPACE_ID`, and the secret `CF_API_TOKEN` (needs KV read).

## Command format

From the owner's address, one change per line:

    checkout HMFWJXSQRM 13:00
    checkin HMFWJXSQRM 3:00 PM

## Checks

- `npm test` covers the sender and DKIM checks.
- Only the first `Authentication-Results` header is trusted.
