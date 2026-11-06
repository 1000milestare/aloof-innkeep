# Command worker (Cloudflare Email Worker)

Turns a command email sent to the command address into a run of the GitHub
Actions sync. The Worker only checks the sender, forwards the mail to Gmail (so
the sync can read it), and starts the workflow. It never interprets the command.

## Command format

Forward or send to the command address from the owner's address, with one line
per change:

    checkout HMFWJXSQRM 13:00
    checkin HMFWJXSQRM 3:00 PM

Only check-in and checkout are supported. Anything else in the message is ignored.

## Setup

1. **Verify the Gmail destination** in Cloudflare Email Routing. Routing rules
   that point to an unverified address stay disabled.
2. **Routing rule:** route `beauty@prytzhouse.com` (or the chosen command
   address) to this Worker.
3. **Config:** set `ALLOWED_SENDER`, `FORWARD_TO`, and `GITHUB_*` in
   `wrangler.toml`, or in the dashboard.
4. **Token:** create a fine-grained GitHub token with Actions: write on
   `1000milestare/aloof-innkeep`, then run `wrangler secret put GITHUB_TOKEN`.
5. **Deploy:** `npm install && npx wrangler deploy`.
6. **Sync side:** set the repository variable `COMMAND_SENDER` to the owner's
   address, so the sync reads commands from that sender only.

## Checks

- `npm test` runs the sender and DKIM checks.
- Only the first `Authentication-Results` header is trusted, since senders can add their own.
- Forwarding keeps the message body. Whether it keeps the original `From` header
  has not been verified. The sync searches by `from:` for commands, so check this
  with a real forward before relying on it.
