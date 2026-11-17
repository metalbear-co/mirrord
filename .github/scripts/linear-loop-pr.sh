#!/usr/bin/env bash
# Routes a pull request opened by the Linear coding agent: asks the team that owns the ticket for a
# review, and settles the Linear issue once the pull request merges or closes unmerged.
#
# Requires $PR_NUMBER, $PR_URL, $REPO, $HEAD_REF and $GH_TOKEN. The review request
# needs $REVIEW_TOKEN, a GitHub token that can see the organization's teams, and is skipped without
# it. The Linear steps need $LINEAR_APP_CLIENT_ID and $LINEAR_APP_CLIENT_SECRET, which
# `linear-token.sh` turns into a token, and are skipped without them.
# shellcheck disable=SC2016 # GraphQL variables are not shell expansions
set -euo pipefail

: "${PR_NUMBER:?}" "${PR_URL:?}" "${REPO:?}" "${HEAD_REF:?}"

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

# The comment on an unmerged close starts with this, which is also how a rerun finds it.
notice="The pull request ${PR_URL} was closed without merging"

live_state() { gh api "repos/$REPO/pulls/$PR_NUMBER" --jq '"\(.state) \(.merged) \(.draft)"'; }

linear_login() {
  token=$(timeout 30s "$(dirname "$0")/linear-token.sh") || return 1
  echo "::add-mask::$token"
}

# GraphQL reports failures in the body with HTTP 200, so the response has to be inspected.
linear() {
  local response
  response=$(curl --fail-with-body --silent --show-error --max-time 30 \
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

read_issue() {
  issue=$(linear 'query($id: String!, $notice: String!) { issue(id: $id) { id state { id type } team { states { nodes { id name type } } } comments(filter: {body: {contains: $notice}}, first: 1) { nodes { id } } } }' \
    "$(jq -n --arg id "$identifier" --arg notice "$notice" '{id: $id, notice: $notice}')") || return 1
  issue_id=$(jq -r '.data.issue.id // empty' <<< "$issue")
  state_id=$(jq -r '.data.issue.state.id // empty' <<< "$issue")
  state_type=$(jq -r '.data.issue.state.type // empty' <<< "$issue")
  noticed=$(jq -r '.data.issue.comments.nodes[0].id // empty' <<< "$issue")
  [ -n "$issue_id" ] && [ -n "$state_id" ] && [ -n "$state_type" ]
}

state_named() {
  jq -r --arg name "$1" '[.data.issue.team.states.nodes[] | select(.name == $name)][0].id // empty' <<< "$issue"
}

move() {
  local updated
  updated=$(linear 'mutation($id: String!, $state: String!) { issueUpdate(id: $id, input: {stateId: $state}) { success } }' \
    "$(jq -n --arg id "$issue_id" --arg state "$1" '{id: $id, state: $state}')") || return 1
  [ "$(jq -r '.data.issueUpdate.success' <<< "$updated")" = true ]
}

request_review() {
  local reviewers
  if [[ $live == *" true" ]]; then
    echo "$identifier is a draft; the review is requested when it is ready"
    return 0
  fi
  # GitHub answers a team request made with the workflow token with 422, because that token
  # cannot see the organization's teams.
  if [ -z "${REVIEW_TOKEN:-}" ]; then
    echo "::warning::no token that can request a team review, so nobody was asked to review $identifier"
    return 0
  fi
  # A team with code review assignment passes the request on to one of its members, so the reviewer
  # can be a person and not the team.
  reviewers=$(GH_TOKEN=$REVIEW_TOKEN gh api -X POST "repos/$REPO/pulls/$PR_NUMBER/requested_reviewers" \
    -f "team_reviewers[]=$team" --jq '(.requested_reviewers | length) + (.requested_teams | length)') || return 1
  if [ "$reviewers" -eq 0 ]; then
    echo "::error::GitHub accepted the request for $team but the pull request has no reviewer"
    return 1
  fi
  echo "requested $team for $identifier"
}

# An open pull request means the work goes on. An issue that an earlier close sent to Todo therefore
# goes back to In Progress when its pull request is reopened or replaced.
resume() {
  local todo target
  linear_login || return 1
  read_issue || return 1
  todo=$(state_named Todo)
  if [ -z "$todo" ] || [ "$state_id" != "$todo" ]; then
    return 0
  fi
  target=$(state_named 'In Progress')
  [ -n "$target" ] || return 0
  move "$target" || return 1
  echo "$identifier has an open pull request, so it is back in In Progress"
}

# The run acts on the state the pull request is in now and not on the event that started it,
# because a run can start late, or again, after later events.
live=$(live_state)

if [[ $live == "open "* ]]; then
  failed=
  request_review || failed=1
  if [ -n "${LINEAR_APP_CLIENT_ID:-}" ] && [ -n "${LINEAR_APP_CLIENT_SECRET:-}" ]; then
    if ! resume; then
      echo "::error::could not check whether $identifier has to go back to In Progress"
      failed=1
    fi
  fi
  [ -z "$failed" ] || exit 1
  exit 0
fi

if [ -z "${LINEAR_APP_CLIENT_ID:-}" ] || [ -z "${LINEAR_APP_CLIENT_SECRET:-}" ]; then
  echo "::warning::no Linear credentials, leaving $identifier as it is"
  exit 0
fi
linear_login

if [[ $live == "closed true "* ]]; then
  if ! read_issue; then
    echo "::error::Linear returned no issue for $identifier"
    exit 1
  fi
  case $state_type in
    completed | canceled | duplicate)
      echo "$identifier is already closed, leaving it"
      exit 0
      ;;
  esac
  target=$(state_named 'Pending Release')
  if [ -z "$target" ]; then
    echo "::warning::the team of $identifier has no Pending Release state"
    exit 0
  fi
  if ! move "$target"; then
    echo "::error::Linear did not move $identifier"
    exit 1
  fi
  echo "moved $identifier to Pending Release"
  exit 0
fi

owner_url=$(linear 'query($id: String!) { user(id: $id) { url } }' "$(jq -n --arg id "$owner" '{id: $id}')" \
  | jq -r '.data.user.url // empty') || owner_url=
if [ -z "$owner_url" ]; then
  echo "::error::could not look up who prioritizes $identifier"
  exit 1
fi

# Linear has no conditional update, so the pull requests are checked as late as possible. Another
# open pull request for the issue from a branch of this repository, such as a replacement or this
# one reopened, means the work goes on.
live=$(live_state)
if [[ $live != "closed false "* ]]; then
  echo "the pull request of $identifier changed state, leaving the issue to that event"
  exit 0
fi
others=$(gh api --paginate "repos/$REPO/pulls?state=open&per_page=100" \
  --jq ".[] | select((.head.repo.full_name // \"\") == \"$REPO\" and (.head.ref | test(\"(^|/)$key(-|\$)\"; \"i\"))) | .number")
if [ -n "$others" ]; then
  echo "$identifier still has an open pull request (#${others//$'\n'/, #}), leaving it"
  exit 0
fi

if ! read_issue; then
  echo "::error::Linear returned no issue for $identifier"
  exit 1
fi
todo=$(state_named Todo)
if [ -z "$todo" ]; then
  echo "::warning::the team of $identifier has no Todo state"
  exit 0
fi

# A rerun after a failure finds the issue in Todo already, or the comment posted already, and only
# does what is missing.
if [ "$state_type" = started ]; then
  if ! move "$todo"; then
    echo "::error::Linear did not move $identifier"
    exit 1
  fi
  echo "moved $identifier to Todo"
elif [ "$state_id" != "$todo" ]; then
  echo "$identifier is not in progress, leaving it"
  exit 0
fi
if [ -n "$noticed" ]; then
  echo "$identifier already carries the comment about this pull request"
  exit 0
fi
posted=$(linear 'mutation($id: String!, $body: String!) { commentCreate(input: {issueId: $id, body: $body}) { success } }' \
  "$(jq -n --arg id "$issue_id" --arg body "$notice, so this issue is back in Todo. ${owner_url} please reprioritize it." '{id: $id, body: $body}')") || posted=
if [ "$(jq -r '.data.commentCreate.success' <<< "${posted:-null}")" != true ]; then
  echo "::error::$identifier is in Todo, but Linear refused the comment that asks for it to be reprioritized; rerun this job to send it"
  exit 1
fi
echo "asked for $identifier to be reprioritized"
