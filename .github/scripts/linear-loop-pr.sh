#!/usr/bin/env bash
# Routes a pull request opened by the Linear coding agent: asks the team that owns the ticket for a
# review, and settles the Linear issue once the pull request merges or closes unmerged.
#
# Requires $ACTION, $PR_NUMBER, $PR_URL, $REPO, $HEAD_REF, $DRAFT and $GH_TOKEN. The Linear step also
# needs $LINEAR_APP_CLIENT_ID and $LINEAR_APP_CLIENT_SECRET, which `linear-token.sh` turns into a
# token; without them only the review request happens.
# shellcheck disable=SC2016 # GraphQL variables are not shell expansions
set -euo pipefail

: "${ACTION:?}" "${PR_NUMBER:?}" "${PR_URL:?}" "${REPO:?}" "${HEAD_REF:?}"

if [[ ! $HEAD_REF =~ ^linear/([a-z]+)-([0-9]+)(-|$) ]]; then
  echo "::warning::branch $HEAD_REF carries no Linear issue key"
  exit 0
fi
prefix=${BASH_REMATCH[1]}
key="$prefix-${BASH_REMATCH[2]}"
identifier=$(printf '%s' "$key" | tr '[:lower:]' '[:upper:]')

# `owner` is the Linear id of the product manager who prioritizes the team's issues.
case $prefix in
  cor) team=core owner=6bbcaf3a-71da-406a-81cc-00a2c79adab3 ;;
  int) team=cloud-integrations owner=c9fbc8c6-7cbd-4959-a288-153adf7d937e ;;
  *) team=rnd owner=c9fbc8c6-7cbd-4959-a288-153adf7d937e ;;
esac

if [ "$ACTION" != closed ]; then
  if [ "${DRAFT:-false}" = true ]; then
    echo "$identifier is a draft; the review is requested when it is ready"
    exit 0
  fi
  # A team with code review assignment passes the request on to one of its members, so the reviewer
  # can be a person and not the team.
  reviewers=$(gh api -X POST "repos/$REPO/pulls/$PR_NUMBER/requested_reviewers" \
    -f "team_reviewers[]=$team" --jq '(.requested_reviewers | length) + (.requested_teams | length)')
  if [ "$reviewers" -eq 0 ]; then
    echo "::error::GitHub accepted the request for $team but the pull request has no reviewer"
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

# Another open pull request for the issue, such as a replacement, means the work goes on.
other_pull_requests() {
  gh api --paginate "repos/$REPO/pulls?state=open&per_page=100" \
    --jq ".[] | select(.head.ref | test(\"(^|/)$key(-|\$)\"; \"i\")) | .number"
}

if [ "$merged" != true ]; then
  others=$(other_pull_requests)
  if [ -n "$others" ]; then
    echo "$identifier still has an open pull request (#${others//$'\n'/, #}), leaving it"
    exit 0
  fi
  # Looked up before the issue moves, so that a failure here leaves a rerun everything to do.
  owner_url=$(linear 'query($id: String!) { user(id: $id) { url } }' "$(jq -n --arg id "$owner" '{id: $id}')" \
    | jq -r '.data.user.url // empty') || owner_url=
  if [ -z "$owner_url" ]; then
    echo "::error::could not look up who prioritizes $identifier"
    exit 1
  fi
fi

# Linear has no conditional update, so the issue is read as late as possible before it is moved.
issue=$(linear 'query($id: String!) { issue(id: $id) { id state { id type } team { states { nodes { id name type } } } } }' \
  "$(jq -n --arg id "$identifier" '{id: $id}')") || exit 1
issue_id=$(jq -r '.data.issue.id // empty' <<< "$issue")
previous=$(jq -r '.data.issue.state.id // empty' <<< "$issue")
state_type=$(jq -r '.data.issue.state.type // empty' <<< "$issue")
if [ -z "$issue_id" ] || [ -z "$previous" ] || [ -z "$state_type" ]; then
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
  target_name='Pending Release'
else
  if [ "$state_type" != started ]; then
    echo "$identifier is not in progress, leaving it"
    exit 0
  fi
  target_name=Todo
fi
target=$(jq -r --arg name "$target_name" '[.data.issue.team.states.nodes[] | select(.name == $name)][0].id // empty' <<< "$issue")
if [ -z "$target" ]; then
  echo "::warning::the team of $identifier has no $target_name state"
  exit 0
fi

move() {
  local updated
  updated=$(linear 'mutation($id: String!, $state: String!) { issueUpdate(id: $id, input: {stateId: $state}) { success } }' \
    "$(jq -n --arg id "$issue_id" --arg state "$1" '{id: $id, state: $state}')") || return 1
  [ "$(jq -r '.data.issueUpdate.success' <<< "$updated")" = true ]
}

if ! move "$target"; then
  echo "::error::Linear did not move $identifier"
  exit 1
fi

# The pull request can be reopened, or replaced by another one, while this runs. A check before the
# move cannot rule that out, so the move is checked afterwards and undone. The event of the newer
# state then settles the issue.
after=$(live_state)
others=
if [ "$merged" != true ]; then
  others=$(other_pull_requests)
fi
if [ "$after" != "$live" ] || [ -n "$others" ]; then
  current=$(linear 'query($id: String!) { issue(id: $id) { state { id } } }' "$(jq -n --arg id "$issue_id" '{id: $id}')" \
    | jq -r '.data.issue.state.id // empty') || current=
  if [ -z "$current" ]; then
    echo "::error::$identifier moved to $target_name although its pull request was reopened or replaced, and Linear did not say where the issue is now"
    exit 1
  fi
  if [ "$current" != "$target" ]; then
    echo "::warning::the pull request of $identifier was reopened or replaced, but so did the issue change, leaving it"
    exit 0
  fi
  if ! move "$previous"; then
    echo "::error::$identifier moved to $target_name although its pull request was reopened or replaced, and Linear did not move it back"
    exit 1
  fi
  echo "the pull request of $identifier was reopened or replaced, so the issue is back where it was"
  exit 0
fi
echo "moved $identifier to $target_name"

if [ "$merged" != true ]; then
  comment="The pull request ${PR_URL} was closed without merging, so this issue is back in Todo. ${owner_url} please reprioritize it."
  posted=$(linear 'mutation($id: String!, $body: String!) { commentCreate(input: {issueId: $id, body: $body}) { success } }' \
    "$(jq -n --arg id "$issue_id" --arg body "$comment" '{id: $id, body: $body}')") || posted=
  if [ "$(jq -r '.data.commentCreate.success' <<< "${posted:-null}")" != true ]; then
    echo "::error::$identifier is back in Todo, but Linear refused the comment that asks for it to be reprioritized"
    exit 1
  fi
fi
