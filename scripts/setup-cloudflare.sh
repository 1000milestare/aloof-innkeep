#!/usr/bin/env bash
# Set up the Cloudflare mail Worker and the GitHub settings that use it.
#
# Automates what the CLIs can do. Steps that need a browser or a secret you
# create yourself are prompted for, or printed at the end (see MANUAL below).
# Secrets are read with hidden input and passed on stdin, never written to disk.
#
# Usage (from the repo root):   bash scripts/setup-cloudflare.sh

set -euo pipefail

REPO="1000milestare/aloof-innkeep"
ACCOUNT_ID="db453fc9a747485e5e0c71ea596004f8"
COMMAND_SENDER="gchirgwin@gmail.com"
WORKER_DIR="$(cd "$(dirname "$0")/../worker" && pwd)"
TOML="$WORKER_DIR/wrangler.toml"

say() { printf '\n==> %s\n' "$*"; }
ask_secret() { local v; read -rsp "$1: " v; echo >&2; printf '%s' "$v"; }

say "Checking tools"
command -v gh >/dev/null || { echo "Install the GitHub CLI (gh) first."; exit 1; }
command -v npx >/dev/null || { echo "Install Node.js (npx) first."; exit 1; }
gh auth status >/dev/null 2>&1 || { echo "Run 'gh auth login' first."; exit 1; }

cd "$WORKER_DIR"
say "Cloudflare login"
if ! npx --yes wrangler whoami 2>/dev/null | grep -q "$ACCOUNT_ID"; then
  echo "Not logged in to account $ACCOUNT_ID. Opening browser login..."
  npx --yes wrangler login
  npx --yes wrangler whoami | grep -q "$ACCOUNT_ID" || {
    echo "Logged in, but not to account $ACCOUNT_ID. Check the account in the browser and rerun."; exit 1; }
fi
echo "Logged in to $ACCOUNT_ID"

say "KV namespace"
if grep -q "REPLACE_WITH_KV_NAMESPACE_ID" "$TOML"; then
  out="$(npx --yes wrangler kv namespace create MAIL 2>&1 || true)"
  ns_id="$(printf '%s' "$out" | grep -oE '[0-9a-f]{32}' | head -1 || true)"
  if [ -z "$ns_id" ]; then
    echo "$out"
    echo "Could not read the namespace id. If MAIL already exists, copy its id from:"
    echo "  npx wrangler kv namespace list"
    read -rp "Paste the namespace id: " ns_id
  fi
  sed -i.bak "s/REPLACE_WITH_KV_NAMESPACE_ID/$ns_id/" "$TOML" && rm -f "$TOML.bak"
  echo "Namespace id: $ns_id (written to wrangler.toml)"
else
  ns_id="$(grep -oE 'id = "[0-9a-f]{32}"' "$TOML" | grep -oE '[0-9a-f]{32}' | head -1)"
  echo "Already set: $ns_id"
fi

say "Worker secret GITHUB_TOKEN"
echo "Needs: fine-grained GitHub token, Actions read/write on $REPO."
gh_token="$(ask_secret "GitHub token")"
printf '%s' "$gh_token" | npx --yes wrangler secret put GITHUB_TOKEN >/dev/null
echo "Stored in the Worker."

say "GitHub repo settings ($REPO)"
gh variable set CF_ACCOUNT_ID --repo "$REPO" --body "$ACCOUNT_ID"
gh variable set CF_KV_NAMESPACE_ID --repo "$REPO" --body "$ns_id"
gh variable set COMMAND_SENDER --repo "$REPO" --body "$COMMAND_SENDER"
echo "Variables set: CF_ACCOUNT_ID, CF_KV_NAMESPACE_ID, COMMAND_SENDER"

echo "Needs: Cloudflare API token with Account > Workers KV Storage > Read"
echo "       and Account > Workers Scripts > Edit (create it at My Profile > API Tokens)."
cf_token="$(ask_secret "Cloudflare API token")"
printf '%s' "$cf_token" | gh secret set CF_API_TOKEN --repo "$REPO"
echo "Secret set: CF_API_TOKEN"

say "Deploying the Worker"
npx --yes wrangler deploy

cat <<MANUAL

==> MANUAL steps (dashboard only)

1. Confirm geoff@chirgwin.com is a verified destination:
   Cloudflare dashboard > prytzhouse.com > Email > Email Routing > Destination addresses.

2. Point the routing rule at the Worker:
   Email > Email Routing > Routing rules > edit the beauty@prytzhouse.com rule.
   Set the action to "Send to a Worker", pick aloof-innkeep-mail, save.
   Leave the catch-all alone.

3. Test:
   - Forward an Airbnb email to beauty@prytzhouse.com; check it reaches geoff@chirgwin.com.
   - Email Routing > Activity log shows the delivery.
   - npx wrangler tail (in worker/) shows "Workflow dispatched".
   - Send: checkout HMFWJXSQRM 13:00   (use a real reservation code) and check the next run.
   - Mail from any other address should be rejected.

Done. Merge PR #24 to deploy the Worker from GitHub.
MANUAL
