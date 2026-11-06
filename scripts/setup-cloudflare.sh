#!/usr/bin/env bash
# Set up the Cloudflare mail Worker and the GitHub settings that use it.
#
# No personal values are stored in the repo. You enter them when prompted, or
# export them first:
#   CLOUDFLARE_ACCOUNT_ID   your Cloudflare account id
#   OWNER_EMAILS            comma-separated addresses you send commands from
#   FORWARD_EMAIL           the verified address that receives a human copy
#
# Secrets are read with hidden input and passed on stdin, never written to disk.
# Generated files (worker/wrangler.local.toml) are git-ignored.
#
# Usage (from the repo root):   bash scripts/setup-cloudflare.sh

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORKER_DIR="$ROOT/worker"
TEMPLATE="$WORKER_DIR/wrangler.toml"
LOCAL_TOML="$WORKER_DIR/wrangler.local.toml"

say() { printf '\n==> %s\n' "$*"; }
ask() { local v; read -rp "$1: " v; printf '%s' "$v"; }
ask_secret() { local v; read -rsp "$1: " v; echo >&2; printf '%s' "$v"; }

say "Checking tools"
command -v gh >/dev/null || { echo "Install the GitHub CLI (gh) first."; exit 1; }
command -v npx >/dev/null || { echo "Install Node.js (npx) first."; exit 1; }
gh auth status >/dev/null 2>&1 || { echo "Run 'gh auth login' first."; exit 1; }

REPO="$(gh repo view --json nameWithOwner --jq .nameWithOwner)"
ORG="${REPO%%/*}"
echo "Repo: $REPO (settings are stored at the org: $ORG)"

ACCOUNT_ID="${CLOUDFLARE_ACCOUNT_ID:-$(ask "Cloudflare account id")}"
OWNER_EMAILS="${OWNER_EMAILS:-$(ask "Owner addresses, comma-separated (commands are accepted from these)")}"
FORWARD_EMAIL="${FORWARD_EMAIL:-$(ask "Verified address for the human copy")}"
export CLOUDFLARE_ACCOUNT_ID="$ACCOUNT_ID"

cd "$WORKER_DIR"

say "Cloudflare login"
if ! npx --yes wrangler whoami 2>/dev/null | grep -q "$ACCOUNT_ID"; then
  echo "Not logged in to account $ACCOUNT_ID. Opening browser login..."
  npx --yes wrangler login
  npx --yes wrangler whoami | grep -q "$ACCOUNT_ID" || {
    echo "Logged in, but not to account $ACCOUNT_ID. Check the account in the browser and rerun."; exit 1; }
fi
echo "Logged in to the account"

say "KV namespace"
ns_id=""
if [ -f "$LOCAL_TOML" ]; then
  ns_id="$(grep -oE 'id = "[0-9a-f]{32}"' "$LOCAL_TOML" | grep -oE '[0-9a-f]{32}' | head -1 || true)"
fi
if [ -z "$ns_id" ]; then
  out="$(npx --yes wrangler kv namespace create MAIL 2>&1 || true)"
  ns_id="$(printf '%s' "$out" | grep -oE '[0-9a-f]{32}' | head -1 || true)"
  if [ -z "$ns_id" ]; then
    echo "$out"
    echo "Could not read the namespace id. If MAIL already exists, find its id with:"
    echo "  npx wrangler kv namespace list"
    ns_id="$(ask "Namespace id")"
  fi
fi
sed "s/REPLACE_WITH_KV_NAMESPACE_ID/$ns_id/" "$TEMPLATE" > "$LOCAL_TOML"
echo "Wrote worker/wrangler.local.toml (git-ignored)"

say "Worker secrets"
printf '%s' "$OWNER_EMAILS" | npx --yes wrangler secret put OWNER_SENDERS --config "$LOCAL_TOML" >/dev/null
printf '%s' "$FORWARD_EMAIL" | npx --yes wrangler secret put FORWARD_TO --config "$LOCAL_TOML" >/dev/null
echo "Needs: fine-grained GitHub token, Actions read/write on $REPO."
gh_token="$(ask_secret "GitHub token for the Worker")"
printf '%s' "$gh_token" | npx --yes wrangler secret put GITHUB_TOKEN --config "$LOCAL_TOML" >/dev/null
echo "Secrets stored in the Worker: OWNER_SENDERS, FORWARD_TO, GITHUB_TOKEN"

say "Org settings ($ORG), shared with $REPO only"
gh variable set CF_ACCOUNT_ID --org "$ORG" --visibility selected --repos "$REPO" --body "$ACCOUNT_ID"
gh variable set CF_KV_NAMESPACE_ID --org "$ORG" --visibility selected --repos "$REPO" --body "$ns_id"
gh variable set COMMAND_SENDER --org "$ORG" --visibility selected --repos "$REPO" --body "$OWNER_EMAILS"
echo "Variables set: CF_ACCOUNT_ID, CF_KV_NAMESPACE_ID, COMMAND_SENDER"

echo "Needs: Cloudflare API token with Account > Workers KV Storage > Read"
echo "       and Account > Workers Scripts > Edit (My Profile > API Tokens)."
cf_token="$(ask_secret "Cloudflare API token")"
printf '%s' "$cf_token" | gh secret set CF_API_TOKEN --org "$ORG" --visibility selected --repos "$REPO"
echo "Secret set: CF_API_TOKEN (org)"

say "Deploying the Worker"
npx --yes wrangler deploy --config "$LOCAL_TOML"

cat <<MANUAL

==> MANUAL steps (dashboard only)

1. Confirm the forward address is a verified destination:
   Cloudflare dashboard > your domain > Email > Email Routing > Destination addresses.

2. Point the command address's routing rule at the Worker:
   Email > Email Routing > Routing rules > edit the rule for the command address.
   Set the action to "Send to a Worker", pick aloof-innkeep-mail, save.
   Leave the catch-all alone.

3. Test:
   - Forward an Airbnb email to the command address; check the human copy arrives.
   - Email Routing > Activity log shows the delivery.
   - npx wrangler tail --config worker/wrangler.local.toml shows "Workflow dispatched".
   - Send a command such as "checkout <real reservation code> 13:00" and check the next run.
   - Mail from any other address should be rejected.
MANUAL
