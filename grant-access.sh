#!/bin/bash
# Grant service account access to a calendar
# Usage: ./grant-access.sh <calendar-id> <service-account-email>

set -e

CALENDAR_ID="${1:?Calendar ID required}"
SERVICE_ACCOUNT_EMAIL="${2:?Service account email required}"

if [ ! -f ".env" ]; then
    echo "Error: .env file not found"
    exit 1
fi

# Get service account JSON path from .env
SERVICE_ACCOUNT_JSON=$(grep GOOGLE_SERVICE_ACCOUNT_JSON .env | cut -d= -f2 | tr -d ' ')

if [ ! -f "$SERVICE_ACCOUNT_JSON" ]; then
    echo "Error: Service account JSON not found at $SERVICE_ACCOUNT_JSON"
    exit 1
fi

# Extract private key and client email
PRIVATE_KEY=$(jq -r '.private_key' "$SERVICE_ACCOUNT_JSON")
CLIENT_EMAIL=$(jq -r '.client_email' "$SERVICE_ACCOUNT_JSON")

# Generate JWT
NOW=$(date +%s)
EXP=$((NOW + 3600))

JWT_HEADER=$(echo -n '{"alg":"RS256","typ":"JWT"}' | base64 | tr -d '\n' | tr '+/' '-_' | tr -d '=')

JWT_PAYLOAD=$(echo -n "{\"iss\":\"$CLIENT_EMAIL\",\"scope\":\"https://www.googleapis.com/auth/calendar\",\"aud\":\"https://oauth2.googleapis.com/token\",\"exp\":$EXP,\"iat\":$NOW}" | base64 | tr -d '\n' | tr '+/' '-_' | tr -d '=')

JWT_SIGNATURE=$(echo -n "$JWT_HEADER.$JWT_PAYLOAD" | openssl dgst -sha256 -sign <(echo -n "$PRIVATE_KEY") | base64 | tr -d '\n' | tr '+/' '-_' | tr -d '=')

JWT="$JWT_HEADER.$JWT_PAYLOAD.$JWT_SIGNATURE"

# Get access token
TOKEN_RESPONSE=$(curl -s -X POST https://oauth2.googleapis.com/token \
  -d "grant_type=urn:ietf:params:oauth:grant-type:jwt-bearer&assertion=$JWT")

ACCESS_TOKEN=$(echo "$TOKEN_RESPONSE" | jq -r '.access_token')

if [ "$ACCESS_TOKEN" = "null" ] || [ -z "$ACCESS_TOKEN" ]; then
    echo "Error: Failed to get access token"
    echo "$TOKEN_RESPONSE"
    exit 1
fi

echo "Got access token"

# Grant access to service account
ACLS_RESPONSE=$(curl -s -X POST \
  "https://www.googleapis.com/calendar/v3/calendars/$(echo -n "$CALENDAR_ID" | jq -sRr @uri)/acl" \
  -H "Authorization: Bearer $ACCESS_TOKEN" \
  -H "Content-Type: application/json" \
  -d "{
    \"role\": \"writer\",
    \"scope\": {
      \"type\": \"user\",
      \"emailAddress\": \"$SERVICE_ACCOUNT_EMAIL\"
    }
  }")

if echo "$ACLS_RESPONSE" | jq -e '.id' > /dev/null 2>&1; then
    echo "✓ Successfully granted access to $SERVICE_ACCOUNT_EMAIL"
    echo "Calendar: $CALENDAR_ID"
else
    echo "Error granting access:"
    echo "$ACLS_RESPONSE"
    exit 1
fi
