#!/usr/bin/env bash
#
# Mints an app actor token for Linear's API and prints it.
#
# Usage: linear-token.sh
#
# Requires $LINEAR_APP_CLIENT_ID and $LINEAR_APP_CLIENT_SECRET. The token acts as the application
# rather than as whoever registered it, and lasts 30 days.

set -euo pipefail

if [ -z "${LINEAR_APP_CLIENT_ID:-}" ] || [ -z "${LINEAR_APP_CLIENT_SECRET:-}" ]; then
  echo "LINEAR_APP_CLIENT_ID and LINEAR_APP_CLIENT_SECRET must both be set" >&2
  exit 1
fi

refused() {
  local detail
  detail=$(jq -r '.error_description // .error // empty' <<< "$1" 2> /dev/null || true)
  printf 'Linear refused to mint a token: %s\n' "${detail:-$1}" >&2
  exit 1
}

if ! response=$(curl --fail-with-body --silent --show-error \
  -X POST https://api.linear.app/oauth/token \
  --data-urlencode grant_type=client_credentials \
  --data-urlencode "client_id=$LINEAR_APP_CLIENT_ID" \
  --data-urlencode "client_secret=$LINEAR_APP_CLIENT_SECRET" \
  --data-urlencode scope=read,write); then
  refused "$response"
fi

token=$(jq -r '.access_token // empty' <<< "$response")

[ -n "$token" ] || refused "$response"

printf '%s\n' "$token"
