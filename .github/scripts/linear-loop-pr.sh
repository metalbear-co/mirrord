#!/usr/bin/env bash
# Routes a pull request opened by the Linear coding agent: asks the team that owns the ticket for a
# review, and sends the Linear issue back to Todo when the pull request closes unmerged. Linear's own
# pull request automations move the issue when the pull request opens or merges.
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

case $prefix in
  cor) team=core ;;
  int) team=cloud-integrations ;;
  *) team=rnd ;;
esac

live_state() { gh api "repos/$REPO/pulls/$PR_NUMBER" --jq '"\(.state) \(.merged) \(.draft)"'; }

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

# The run acts on the state the pull request is in now and not on the event that started it,
# because a run can start late, or again, after later events.
live=$(live_state)

if [[ $live == "open "* ]]; then
  if [[ $live == *" true" ]]; then
    echo "$identifier is a draft; the review is requested when it is ready"
  elif [ -z "${REVIEW_TOKEN:-}" ]; then
    # GitHub answers a team request made with the workflow token with 422, because that token
    # cannot see the organization's teams.
    echo "::warning::no token that can request a team review, so nobody was asked to review $identifier"
  else
    # A team with code review assignment passes the request on to one of its members, so the reviewer
    # can be a person and not the team.
    reviewers=$(GH_TOKEN=$REVIEW_TOKEN gh api -X POST "repos/$REPO/pulls/$PR_NUMBER/requested_reviewers" \
      -f "team_reviewers[]=$team" --jq '(.requested_reviewers | length) + (.requested_teams | length)')
    if [ "$reviewers" -eq 0 ]; then
      echo "::error::GitHub accepted the request for $team but the pull request has no reviewer"
      exit 1
    fi
    echo "requested $team for $identifier"
  fi
  exit 0
fi

if [[ $live == "closed true "* ]]; then
  echo "the pull request of $identifier merged, leaving the issue to Linear's pull request automation"
  exit 0
fi

if [ -z "${LINEAR_APP_CLIENT_ID:-}" ] || [ -z "${LINEAR_APP_CLIENT_SECRET:-}" ]; then
  echo "::warning::no Linear credentials, leaving $identifier as it is"
  exit 0
fi
token=$(timeout 30s "$(dirname "$0")/linear-token.sh")
echo "::add-mask::$token"

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

# The comment on an unmerged close starts with this, which is also how a rerun finds it.
notice="The pull request ${PR_URL} was closed without merging"

issue=$(linear 'query($id: String!, $notice: String!) { issue(id: $id) { id state { id type } team { states { nodes { id name } } triageResponsibility { currentUser { url } } } comments(filter: {body: {contains: $notice}}, first: 1) { nodes { id } } } }' \
  "$(jq -n --arg id "$identifier" --arg notice "$notice" '{id: $id, notice: $notice}')") || issue=null
issue_id=$(jq -r '.data.issue.id // empty' <<< "$issue")
state_id=$(jq -r '.data.issue.state.id // empty' <<< "$issue")
state_type=$(jq -r '.data.issue.state.type // empty' <<< "$issue")
noticed=$(jq -r '.data.issue.comments.nodes[0].id // empty' <<< "$issue")
triager_url=$(jq -r '.data.issue.team.triageResponsibility.currentUser.url // empty' <<< "$issue")
if [ -z "$issue_id" ] || [ -z "$state_id" ] || [ -z "$state_type" ]; then
  echo "::error::Linear returned no issue for $identifier"
  exit 1
fi
todo=$(jq -r '[.data.issue.team.states.nodes[] | select(.name == "Todo")][0].id // empty' <<< "$issue")
if [ -z "$todo" ]; then
  echo "::warning::the team of $identifier has no Todo state"
  exit 0
fi

# A rerun after a failure finds the issue in Todo already, or the comment posted already, and only
# does what is missing.
if [ "$state_type" = started ]; then
  moved=$(linear 'mutation($id: String!, $state: String!) { issueUpdate(id: $id, input: {stateId: $state}) { success } }' \
    "$(jq -n --arg id "$issue_id" --arg state "$todo" '{id: $id, state: $state}')") || moved=null
  if [ "$(jq -r '.data.issueUpdate.success' <<< "$moved")" != true ]; then
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
  "$(jq -n --arg id "$issue_id" --arg body "$notice, so this issue is back in Todo. ${triager_url:-Someone} please reprioritize it." '{id: $id, body: $body}')") || posted=null
if [ "$(jq -r '.data.commentCreate.success' <<< "$posted")" != true ]; then
  echo "::error::$identifier is in Todo, but Linear refused the comment that asks for it to be reprioritized; rerun this job to send it"
  exit 1
fi
echo "asked for $identifier to be reprioritized"
