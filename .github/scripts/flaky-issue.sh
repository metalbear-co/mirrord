#!/usr/bin/env bash
#
# Reports, files, or updates the Linear issue tracking a test that retries on `main`.
#
# Keeps one issue per test, found by the attachment the report leaves on it rather than by its
# title, so retitling an issue in Linear does not orphan it. With a <body-file> it files that issue
# when none is open and comments on it when one is; without one it only looks the issue up, printing
# nothing when none is open.
#
# Usage: flaky-issue.sh <repo> <team-key> <test> [<body-file>]
#
# Prints the issue identifier. Requires $LINEAR_ACCESS_KEY.

set -euo pipefail

repo=$1
team_key=$2
name=$3
body_file=${4:-}

label=flake
title="Flaky test in ${repo#*/}: $name"

# Doubles as the attachment's link and as the key the next run finds the issue by, so it has to be
# derived from nothing but the test itself.
key_url="https://github.com/$repo/search?q=$(jq -rn --arg q "$name" '$q | @uri')&type=code"

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

existing=$(api "$(jq -n --arg url "$key_url" '{
  query: "query($url: String!) {
    attachmentsForURL(url: $url) {
      nodes { issue { id identifier state { type } team { key } } }
    }
  }",
  variables: { url: $url }
}')" | jq -r --arg key "$team_key" '
  [.data.attachmentsForURL.nodes[]?.issue
   | select(.team.key == $key)
   | select(.state.type != "completed" and .state.type != "canceled")][0] // empty
  | @json')

if [ -z "$body_file" ]; then
  printf '%s' "$existing" | jq -r '.identifier // empty'
  exit 0
fi

body=$(cat "$body_file")

if [ -n "$existing" ]; then
  identifier=$(printf '%s' "$existing" | jq -r .identifier)
  mutation=$(jq -n --arg id "$(printf '%s' "$existing" | jq -r .id)" --arg body "$body" '{
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

label_id=$(api "$(jq -n --arg name "$label" '{
  query: "query($name: String!) {
    issueLabels(filter: { name: { eq: $name } }) { nodes { id team { key } } }
  }",
  variables: { name: $name }
}')" | jq -r --arg key "$team_key" '
  [.data.issueLabels.nodes[]? | select(.team == null or .team.key == $key)][0].id // empty')

if [ -z "$label_id" ]; then
  echo "::warning::no Linear label named $label, filing $title without it" >&2
fi

created=$(api "$(jq -n \
  --arg team "$team_id" \
  --arg title "$title" \
  --arg body "$body" \
  --arg label "$label_id" '{
  query: "mutation($team: String!, $title: String!, $body: String!, $labels: [String!]) {
    issueCreate(input: {
      teamId: $team, title: $title, description: $body, priority: 1, labelIds: $labels
    }) {
      success
      issue { id identifier }
    }
  }",
  variables: {
    team: $team,
    title: $title,
    body: $body,
    labels: (if $label == "" then [] else [$label] end)
  }
}')" | jq -r '.data.issueCreate | select(.success == true) | .issue // empty')

if [ -z "$created" ]; then
  echo "Linear rejected the new issue" >&2
  exit 1
fi

attached=$(api "$(jq -n \
  --arg id "$(printf '%s' "$created" | jq -r .id)" \
  --arg url "$key_url" \
  --arg title "Flaky test" \
  --arg name "$name" \
  --arg repo "$repo" '{
  query: "mutation($id: String!, $url: String!, $title: String!, $name: String!, $metadata: JSONObject!) {
    attachmentCreate(input: {
      issueId: $id, url: $url, title: $title, subtitle: $name, metadata: $metadata
    }) { success }
  }",
  variables: {
    id: $id,
    url: $url,
    title: $title,
    name: $name,
    metadata: { source: "flaky-test-report", repository: $repo, test: $name }
  }
}')" | jq -r '.data.attachmentCreate.success')

# Without the attachment the next run cannot find this issue and files a duplicate.
if [ "$attached" != "true" ]; then
  echo "Linear rejected the attachment keying $title" >&2
  exit 1
fi

printf '%s' "$created" | jq -r .identifier
