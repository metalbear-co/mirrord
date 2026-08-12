#!/usr/bin/env bash
#
# Reports, files, or updates the Linear issue tracking a test that retries on `main`.
#
# Keeps one issue per test: with a <body-file> it files that issue when none is open and comments on
# it when one is, so a daily report updates a single issue rather than opening one per run. Without
# a <body-file> it only looks the issue up, printing nothing when none is open.
#
# Usage: flaky-issue.sh <repo> <team-key> <test> [<body-file>]
#
# Prints the issue identifier. Requires $LINEAR_ACCESS_KEY.

set -euo pipefail

repo=$1
team_key=$2
name=$3
body_file=${4:-}

title="Flaky test in $repo: $name"

# GraphQL reports failures in the body with HTTP 200, so the response has to be inspected rather
# than left to curl's status handling.
api() {
  local response
  response=$(curl --fail-with-body --silent --show-error \
    -X POST https://api.linear.app/graphql \
    -H "Authorization: $LINEAR_ACCESS_KEY" \
    -H 'Content-Type: application/json' \
    --data "$1")

  if printf '%s' "$response" | jq -e 'has("errors")' > /dev/null; then
    printf 'Linear API error: %s\n' "$(printf '%s' "$response" | jq -c '.errors')" >&2
    return 1
  fi

  printf '%s' "$response"
}

query=$(jq -n --arg title "$title" --arg key "$team_key" '{
  query: "query($title: String!, $key: String!) {
    issues(filter: { title: { eq: $title }, team: { key: { eq: $key } } }) {
      nodes { id identifier state { type } }
    }
  }",
  variables: { title: $title, key: $key }
}')

existing=$(api "$query" | jq -r '
  [.data.issues.nodes[]? | select(.state.type != "completed" and .state.type != "canceled")][0] // empty
  | @json')

if [ -z "$body_file" ]; then
  printf '%s' "$existing" | jq -r '.identifier // empty'
  exit 0
fi

body=$(cat "$body_file")

if [ -n "$existing" ]; then
  id=$(printf '%s' "$existing" | jq -r .id)
  identifier=$(printf '%s' "$existing" | jq -r .identifier)
  mutation=$(jq -n --arg id "$id" --arg body "$body" '{
    query: "mutation($id: String!, $body: String!) {
      commentCreate(input: { issueId: $id, body: $body }) { success }
    }",
    variables: { id: $id, body: $body }
  }')
  if [ "$(api "$mutation" | jq -r '.data.commentCreate.success')" != "true" ]; then
    echo "Linear rejected the comment on $identifier" >&2
    exit 1
  fi

  echo "$identifier"
  exit 0
fi

team_id=$(api "$(jq -n --arg key "$team_key" '{
  query: "query($key: String!) { teams(filter: { key: { eq: $key } }) { nodes { id } } }",
  variables: { key: $key }
}')" | jq -r '.data.teams.nodes[0].id // empty')

if [ -z "$team_id" ]; then
  echo "no Linear team with key $team_key" >&2
  exit 1
fi

mutation=$(jq -n --arg team "$team_id" --arg title "$title" --arg body "$body" '{
  query: "mutation($team: String!, $title: String!, $body: String!) {
    issueCreate(input: { teamId: $team, title: $title, description: $body, priority: 1 }) {
      success
      issue { identifier }
    }
  }",
  variables: { team: $team, title: $title, body: $body }
}')

created=$(api "$mutation" | jq -r '.data.issueCreate | select(.success == true) | .issue.identifier // empty')

if [ -z "$created" ]; then
  echo "Linear rejected the new issue" >&2
  exit 1
fi

echo "$created"
