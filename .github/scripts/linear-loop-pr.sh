#!/usr/bin/env bash
# Routes a pull request opened by the Linear coding agent: asks the team that owns the ticket for a
# review, and settles the Linear issue once the pull request merges or closes unmerged.
#
# Requires $ACTION, $PR_NUMBER, $PR_URL, $REPO, $HEAD_REF, $DRAFT and $GH_TOKEN. The Linear step also
# needs $LINEAR_APP_CLIENT_ID and $LINEAR_APP_CLIENT_SECRET, which `linear-token.sh` turns into a
# token; without them only the review request happens.
# shellcheck disable=SC2016 # GraphQL variables are not shell expansions
set -euo pipefail

: "${ACTION:?}" "${PR_NUMBER:?}" "${REPO:?}" "${HEAD_REF:?}"

if [[ ! $HEAD_REF =~ ^linear/([a-z]+)-([0-9]+)(-|$) ]]; then
  echo "::warning::branch $HEAD_REF carries no Linear issue key"
  exit 0
fi
prefix=${BASH_REMATCH[1]}
identifier="$(printf '%s' "$prefix" | tr '[:lower:]' '[:upper:]')-${BASH_REMATCH[2]}"

case $prefix in
  cor) team=core ;;
  int) team=cloud-integrations ;;
  *) team=rnd ;;
esac

if [ "$ACTION" != closed ]; then
  if [ "${DRAFT:-false}" = true ]; then
    echo "$identifier is a draft; the review is requested when it is ready"
    exit 0
  fi
  requested=$(gh api -X POST "repos/$REPO/pulls/$PR_NUMBER/requested_reviewers" \
    -f "team_reviewers[]=$team" --jq '[.requested_teams[].slug] | join(",")')
  if [[ ",$requested," != *",$team,"* ]]; then
    echo "::error::GitHub accepted the request but $team is not among the requested teams ($requested)"
    exit 1
  fi
  echo "requested $team for $identifier"
  exit 0
fi

# The event may be stale: the pull request can have been reopened since it was queued.
live_state() { gh api "repos/$REPO/pulls/$PR_NUMBER" --jq '"\(.state) \(.merged)"'; }
live=$(live_state)
if [[ $live != "closed "* ]]; then
  echo "$identifier was reopened, leaving it"
  exit 0
fi
merged=${live#closed }

if [ -z "${LINEAR_APP_CLIENT_ID:-}" ] || [ -z "${LINEAR_APP_CLIENT_SECRET:-}" ]; then
  echo "::warning::no Linear credentials, leaving $identifier as it is"
  exit 0
fi
token=$("$(dirname "$0")/linear-token.sh")
echo "::add-mask::$token"

# GraphQL reports failures in the body with HTTP 200, so the response has to be inspected.
linear() {
  local response
  response=$(curl --fail-with-body --silent --show-error \
    -X POST https://api.linear.app/graphql \
    -H "Authorization: Bearer $token" \
    -H 'Content-Type: application/json' \
    --data "$(jq -n --arg query "$1" --argjson variables "$2" '{query: $query, variables: $variables}')") || return 1
  if jq -e '.errors' > /dev/null <<< "$response"; then
    printf 'Linear API error: %s\n' "$(jq -c '.errors' <<< "$response")" >&2
    return 1
  fi
  printf '%s' "$response"
}

issue=$(linear 'query($id: String!) { issue(id: $id) { id state { type } team { states { nodes { id name type } } } } }' \
  "$(jq -n --arg id "$identifier" '{id: $id}')") || exit 1
issue_id=$(jq -r '.data.issue.id // empty' <<< "$issue")
state_type=$(jq -r '.data.issue.state.type // empty' <<< "$issue")
if [ -z "$issue_id" ] || [ -z "$state_type" ]; then
  echo "::error::Linear returned no issue for $identifier"
  exit 1
fi

if [ "$merged" = true ]; then
  case $state_type in
    completed | canceled | duplicate)
      echo "$identifier is already closed, leaving it"
      exit 0
      ;;
  esac
  target=$(jq -r '[.data.issue.team.states.nodes[] | select(.name == "Pending Release")][0].id // empty' <<< "$issue")
  if [ -z "$target" ]; then
    echo "::warning::the team of $identifier has no Pending Release state"
    exit 0
  fi
  comment=
else
  if [ "$state_type" != started ]; then
    echo "$identifier is not in progress, leaving it"
    exit 0
  fi
  target=$(jq -r '[.data.issue.team.states.nodes[] | select(.type == "triage")][0].id // empty' <<< "$issue")
  if [ -z "$target" ]; then
    echo "::warning::the team of $identifier has no Triage state"
    exit 0
  fi
  comment="The pull request was closed without merging, so this issue is back in Triage for reprioritization: ${PR_URL}"
fi

if [[ $(live_state) != "closed "* ]]; then
  echo "$identifier was reopened, leaving it"
  exit 0
fi
updated=$(linear 'mutation($id: String!, $state: String!) { issueUpdate(id: $id, input: {stateId: $state}) { success } }' \
  "$(jq -n --arg id "$issue_id" --arg state "$target" '{id: $id, state: $state}')") || exit 1
if [ "$(jq -r '.data.issueUpdate.success' <<< "$updated")" != true ]; then
  echo "::error::Linear did not move $identifier"
  exit 1
fi
echo "moved $identifier"

# The explanation is a courtesy: the issue has moved, and a rerun would no longer find it in progress.
if [ -n "$comment" ]; then
  posted=$(linear 'mutation($id: String!, $body: String!) { commentCreate(input: {issueId: $id, body: $body}) { success } }' \
    "$(jq -n --arg id "$issue_id" --arg body "$comment" '{id: $id, body: $body}')") || posted=
  if [ "$(jq -r '.data.commentCreate.success' <<< "${posted:-null}")" != true ]; then
    echo "::warning::could not comment on $identifier"
  fi
fi
