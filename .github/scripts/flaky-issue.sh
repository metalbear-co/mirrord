#!/usr/bin/env bash
#
# Files the Linear issue tracking a test that retries on `main`, or counts a repeat sighting on the
# issue already tracking it.
#
# Keeps one issue per test, found by the attachment the report leaves on it rather than by its title
# or its team, so neither retitling an issue nor moving it to another team orphans it. A test whose
# issue is still open has its tally folded into that issue and a comment left on it, and is reported
# as a repeat: the channel hears about first sightings, the issue's followers about every one.
#
# A test already tracked by an open issue is counted on it however rarely it flaked, so <threshold>
# gates filing a new issue and nothing else.
#
# Usage: flaky-issue.sh <repo> <team-key> <package> <test> <retries> <threshold> <run-url>
#
# Prints `new\t<identifier>\t<url>`, `repeat\t<identifier>\t<url>`, `stale\t<identifier>\t<url>` for a
# repeat whose tally could not be advanced, or `below\t\t` for a test that flaked too rarely to be worth
# an issue of its own. Requires $LINEAR_APP_TOKEN, which `linear-token.sh` mints.

set -euo pipefail

repo=$1
team_key=$2
package=$3
name=$4
retries=$5
threshold=$6
run_url=$7

short=${repo#*/}

# Trailing segments only. The qualified pair is unwieldy on a line and the tail is what a human
# recognises a test by; the attachment keeps the whole of it. Package and test are formatted apart,
# being two names rather than one path.
display="\`${package##*::}\`/\`${name##*::}\`"
title="Fix flaky test $display in $short"
seen=$(date -u +%Y-%m-%dT%H:%M:%SZ)

repo_url="https://github.com/$repo"

# The key the next run finds this issue by, and nothing else: it names the test in full so that two
# tests sharing a trailing segment stay apart, and it stays put however the issue is retitled or
# moved.
key_url="flaky-test://$repo/$package/$name"

key_title="Tracking key"

# GraphQL reports failures in the body with HTTP 200, so the response has to be inspected rather
# than left to curl's status handling.
api() {
  local response
  response=$(curl --fail-with-body --silent --show-error \
    -X POST https://api.linear.app/graphql \
    -H "Authorization: Bearer $LINEAR_APP_TOKEN" \
    -H 'Content-Type: application/json' \
    --data "$1")

  if printf '%s' "$response" | jq -e 'has("errors")' > /dev/null; then
    printf 'Linear API error: %s\n' "$(printf '%s' "$response" | jq -c '.errors')" >&2
    return 1
  fi

  printf '%s' "$response"
}

# JUnit's `classname` is nextest's binary id, so the pair the report carries is exactly what a
# filterset needs to select this test and nothing else. A binary id leads with the package that owns
# it, which narrows the build from the whole workspace to the one crate.
run_command="cargo nextest run -p ${package%%::*} -E 'binary_id(=$package) and test(=$name)'"

# Rewritten on every sighting, which is what the note at the foot warns anyone editing it about.
body() {
  printf 'Failed %s× across all observed runs.\n\n### Running in isolation\n\n```\n%s\n```\n\n_This issue was generated automatically. Do not edit by hand, as it may be overridden._' \
    "$1" "$run_command"
}

metadata() {
  jq -n --arg repo "$repo" --arg package "$package" --arg test "$name" --arg seen "$seen" \
    --argjson occurrences "$1" \
    '{source: "flaky-test-report", repository: $repo, package: $package, test: $test,
      occurrences: $occurrences, lastSeen: $seen}'
}

# Open states are listed rather than closed ones: Linear has a `duplicate` type alongside `completed`
# and `canceled`, and counting sightings onto an issue somebody closed as a duplicate would leave the
# surviving issue untouched.
existing=$(api "$(jq -n --arg url "$key_url" '{
  query: "query($url: String!) {
    attachmentsForURL(url: $url) {
      nodes { id metadata issue { id identifier url state { type } } }
    }
  }",
  variables: { url: $url }
}')" | jq -r '
  [.data.attachmentsForURL.nodes[]?
   | select(.issue.state.type as $type
            | ["triage", "backlog", "unstarted", "started"] | index($type))
   | {attachment: .id, occurrences: (.metadata.occurrences // 1),
      issue: .issue.id, identifier: .issue.identifier, url: .issue.url}][0] // empty
  | @json')

if [ -n "$existing" ]; then
  identifier=$(jq -r .identifier <<< "$existing")
  occurrences=$(( $(jq -r .occurrences <<< "$existing") + retries ))

  # The tally sits on top of an issue that already exists and already says what is wrong, so
  # failing to refresh it is not worth failing the report over.
  updated=$(api "$(jq -n \
    --arg id "$(jq -r .issue <<< "$existing")" \
    --arg body "$(body "$occurrences")" '{
    query: "mutation($id: String!, $body: String!) {
      issueUpdate(id: $id, input: { description: $body }) { success }
    }",
    variables: { id: $id, body: $body }
  }')" | jq -r '.data.issueUpdate.success') || updated=false

  if [ "$updated" != "true" ]; then
    echo "::warning::could not refresh the tally on $identifier" >&2
  fi

  counted=$(api "$(jq -n \
    --arg id "$(jq -r .attachment <<< "$existing")" \
    --arg title "$key_title" \
    --arg subtitle "$package/$name" \
    --argjson metadata "$(metadata "$occurrences")" '{
    query: "mutation($id: String!, $title: String!, $subtitle: String!, $metadata: JSONObject!) {
      attachmentUpdate(id: $id, input: { title: $title, subtitle: $subtitle, metadata: $metadata }) { success }
    }",
    variables: { id: $id, title: $title, subtitle: $subtitle, metadata: $metadata }
  }')" | jq -r '.data.attachmentUpdate.success') || counted=false

  state=repeat

  if [ "$counted" != "true" ]; then
    echo "::warning::could not count this sighting on $identifier" >&2
    state=stale
  fi

  # Posted last, so that anyone following the issue arrives at a body already carrying this sighting.
  # The channel stays quiet for repeats; this is how the people who care about a given test hear.
  noted=$(api "$(jq -n --arg id "$(jq -r .issue <<< "$existing")" \
    --arg body "$(printf 'Failed again in [this run](%s). Now %s× across all observed runs.' \
      "$run_url" "$occurrences")" '{
    query: "mutation($id: String!, $body: String!) {
      commentCreate(input: { issueId: $id, body: $body }) { success }
    }",
    variables: { id: $id, body: $body }
  }')" | jq -r '.data.commentCreate.success') || noted=false

  if [ "$noted" != "true" ]; then
    echo "::warning::could not note this sighting on $identifier" >&2
  fi

  printf '%s\t%s\t%s\n' "$state" "$identifier" "$(jq -r .url <<< "$existing")"
  exit 0
fi

if [ "$retries" -lt "$threshold" ]; then
  printf 'below\t\t\n'
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

label_ids='[]'

for label in tech-debt flaky-test; do
  label_id=$(api "$(jq -n --arg name "$label" '{
    query: "query($name: String!) {
      issueLabels(filter: { name: { eq: $name } }) { nodes { id team { key } } }
    }",
    variables: { name: $name }
  }')" | jq -r --arg key "$team_key" '
    [.data.issueLabels.nodes[]? | select(.team == null or .team.key == $key)][0].id // empty')

  if [ -z "$label_id" ]; then
    echo "::warning::no Linear label named $label, filing without it" >&2
    continue
  fi

  label_ids=$(jq -c --arg id "$label_id" '. + [$id]' <<< "$label_ids")
done

created=$(api "$(jq -n \
  --arg team "$team_id" \
  --arg title "$title" \
  --arg body "$(body "$retries")" \
  --argjson labels "$label_ids" '{
  query: "mutation($team: String!, $title: String!, $body: String!, $labels: [String!]) {
    issueCreate(input: {
      teamId: $team, title: $title, description: $body, priority: 2, labelIds: $labels
    }) {
      success
      issue { id identifier url }
    }
  }",
  variables: { team: $team, title: $title, body: $body, labels: $labels }
}')" | jq -r '.data.issueCreate | select(.success == true) | .issue // empty')

if [ -z "$created" ]; then
  echo "Linear rejected the new issue" >&2
  exit 1
fi

issue_id=$(jq -r .id <<< "$created")

# Context for whoever opens the issue. Linear cannot move an attachment's URL, so the run is the one
# that first caught this test rather than the newest, and neither is touched again.
for link in "Repository|$repo_url" "Run|$run_url"; do
  linked=$(api "$(jq -n --arg id "$issue_id" --arg title "${link%%|*}" --arg url "${link#*|}" '{
    query: "mutation($id: String!, $url: String!, $title: String!) {
      attachmentCreate(input: { issueId: $id, url: $url, title: $title }) { success }
    }",
    variables: { id: $id, url: $url, title: $title }
  }')" | jq -r '.data.attachmentCreate.success') || linked=false

  if [ "$linked" != "true" ]; then
    echo "::warning::could not attach ${link%%|*} to the issue" >&2
  fi
done

attached=$(api "$(jq -n \
  --arg id "$issue_id" \
  --arg url "$key_url" \
  --arg title "$key_title" \
  --arg subtitle "$package/$name" \
  --argjson metadata "$(metadata "$retries")" '{
  query: "mutation($id: String!, $url: String!, $title: String!, $subtitle: String!, $metadata: JSONObject!) {
    attachmentCreate(input: {
      issueId: $id, url: $url, title: $title, subtitle: $subtitle, metadata: $metadata
    }) { success }
  }",
  variables: { id: $id, url: $url, title: $title, subtitle: $subtitle, metadata: $metadata }
}')" | jq -r '.data.attachmentCreate.success')

# This attachment is the only thing the next run finds this issue by, so an issue that failed to get
# one is unreachable and would be filed again every run. Trash it, leaving the reason behind for
# whoever finds it there, rather than let it accumulate copies.
if [ "$attached" != "true" ]; then
  echo "Linear rejected the attachment keying this issue, trashing it" >&2

  note='Filed by the flaky test report, which then failed to attach the key it finds this issue by.
Trashed so the next report can file a clean one. The test is still flaky.'

  api "$(jq -n --arg id "$issue_id" --arg body "$note" '{
    query: "mutation($id: String!, $body: String!) {
      commentCreate(input: { issueId: $id, body: $body }) { success }
    }",
    variables: { id: $id, body: $body }
  }')" > /dev/null || true

  trashed=$(api "$(jq -n --arg id "$issue_id" '{
    query: "mutation($id: String!) { issueDelete(id: $id) { success } }",
    variables: { id: $id }
  }')" | jq -r '.data.issueDelete.success') || trashed=false

  if [ "$trashed" != "true" ]; then
    echo "$title is left unkeyed in Linear and will be filed again on the next flake" >&2
  fi

  exit 1
fi

printf 'new\t%s\t%s\n' "$(jq -r .identifier <<< "$created")" "$(jq -r .url <<< "$created")"
