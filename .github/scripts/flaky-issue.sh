#!/usr/bin/env bash
#
# Files the Linear issue tracking a test that retries on `main`, or notes a repeat sighting on the
# issue already tracking it.
#
# Keeps one issue per test, found by the attachment the report leaves on it rather than by its title
# or its team, so neither retitling an issue nor moving it to another team orphans it. Every
# sighting, the first included, is a comment on that issue naming the run and, for a parametrized
# test, the arguments of the cases that retried, with their captured output attached: the channel
# hears about first sightings, the issue's followers about every one.
#
# A test already tracked by an open issue is noted on it however rarely it flaked, so <threshold>
# gates filing a new issue and nothing else.
#
# Usage: flaky-issue.sh <repo> <team-key> <package> <test> <retries> <threshold> <run-url> <dir>
#
# Reads the sighting from <dir> as `flaky-tests.sh` writes it: the captured output in `output.log`,
# and the table of retried cases in `cases.md`, empty for a test that is not parametrized.
#
# Prints `new\t<identifier>\t<url>`, `repeat\t<identifier>\t<url>`, or `below\t\t` for a test that
# flaked too rarely to be worth an issue of its own. Exits 2 after printing the issue when this
# sighting could not be noted on it, which leaves the issue without any record of it. Requires
# $LINEAR_APP_TOKEN, which `linear-token.sh` mints.

set -euo pipefail

repo=$1
team_key=$2
package=$3
name=$4
retries=$5
threshold=$6
run_url=$7
log=$8/output.log
cases=$8/cases.md

short=${repo#*/}

# The crate and the test's trailing segment. The qualified pair is unwieldy on a line and the tail
# is what a human recognises a test by; the attachment keeps the whole of it. Crate and test are
# formatted apart, being two names rather than one path.
display="\`${package%%::*}\`/\`${name##*::}\`"
title="Fix flaky test $display in $short"

# The key the next run finds this issue by, and nothing else: it names the test in full so that two
# tests sharing a trailing segment stay apart, and it stays put however the issue is retitled or
# moved.
key_url="flaky-test://$repo/$package/$name"

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

# JUnit's `classname` is nextest's binary id, which together with the test selects it and nothing
# else: a parametrized test through every case rstest declares under it, a plain one by its name. A
# binary id leads with the package that owns it, which narrows the build from the whole workspace to
# the one crate.
if [ -s "$cases" ]; then
  filter="test(/^$name::/)"
else
  filter="test(=$name)"
fi

body=$(printf '### Running in isolation\n\n```\n%s\n```' \
  "cargo nextest run -p ${package%%::*} -E 'binary_id(=$package) and $filter'")

upload() {
  local filename size target
  local type=text/plain
  local headers=()

  filename="${name##*::}.log"
  size=$(wc -c < "$log")

  target=$(api "$(jq -n --arg type "$type" --arg filename "$filename" --argjson size "$size" '{
    query: "mutation($type: String!, $filename: String!, $size: Int!) {
      fileUpload(contentType: $type, filename: $filename, size: $size) {
        success
        uploadFile { uploadUrl assetUrl headers { key value } }
      }
    }",
    variables: { type: $type, filename: $filename, size: $size }
  }')" | jq -c '.data.fileUpload | select(.success == true) | .uploadFile // empty')

  if [ -z "$target" ]; then
    return 1
  fi

  while IFS= read -r header; do
    headers+=(-H "$header")
  done < <(jq -r '.headers[] | "\(.key): \(.value)"' <<< "$target")

  curl --fail-with-body --silent --show-error \
    -X PUT "$(jq -r .uploadUrl <<< "$target")" \
    -H "Content-Type: $type" \
    -H 'Cache-Control: public, max-age=31536000' \
    "${headers[@]}" \
    --data-binary "@$log" > /dev/null || return 1

  printf '[%s](%s)' "$filename" "$(jq -r .assetUrl <<< "$target")"
}

note() {
  local id=$1
  local comment attachment

  comment=$(printf 'Failed in [this run](%s).' "$run_url")

  if [ -s "$cases" ]; then
    comment+=$'\n\n'$(< "$cases")
  fi

  # The output only adds to a sighting the comment already records, so losing it is not worth
  # losing the comment over.
  if attachment=$(upload); then
    comment+=$'\n\n'$attachment
  else
    echo "::warning::could not attach the captured output of $package/$name" >&2
  fi

  api "$(jq -n --arg id "$id" --arg body "$comment" '{
    query: "mutation($id: String!, $body: String!) {
      commentCreate(input: { issueId: $id, body: $body }) { success }
    }",
    variables: { id: $id, body: $body }
  }')" | jq -e '.data.commentCreate.success' > /dev/null
}

# Open states are listed rather than closed ones: Linear has a `duplicate` type alongside `completed`
# and `canceled`, and noting sightings on an issue somebody closed as a duplicate would leave the
# surviving issue untouched.
existing=$(api "$(jq -n --arg url "$key_url" '{
  query: "query($url: String!) {
    attachmentsForURL(url: $url) {
      nodes { issue { id identifier url state { type } } }
    }
  }",
  variables: { url: $url }
}')" | jq -r '
  [.data.attachmentsForURL.nodes[]?.issue
   | select(.state.type as $type | ["triage", "backlog", "unstarted", "started"] | index($type))][0]
  // empty
  | @json')

if [ -n "$existing" ]; then
  printf 'repeat\t%s\t%s\n' "$(jq -r .identifier <<< "$existing")" "$(jq -r .url <<< "$existing")"
  note "$(jq -r .id <<< "$existing")" || exit 2
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
  --arg body "$body" \
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

attached=$(api "$(jq -n \
  --arg id "$issue_id" \
  --arg url "$key_url" \
  --arg subtitle "$package/$name" \
  --arg repo "$repo" \
  --arg package "$package" \
  --arg test "$name" '{
  query: "mutation($id: String!, $url: String!, $subtitle: String!, $metadata: JSONObject!) {
    attachmentCreate(input: {
      issueId: $id, url: $url, title: \"Tracking key\", subtitle: $subtitle, metadata: $metadata
    }) { success }
  }",
  variables: {
    id: $id, url: $url, subtitle: $subtitle,
    metadata: { source: "flaky-test-report", repository: $repo, package: $package, test: $test }
  }
}')" | jq -r '.data.attachmentCreate.success') || attached=false

# This attachment is the only thing the next run finds this issue by, so an issue that failed to get
# one is unreachable and would be filed again every run. Trash it, leaving the reason behind for
# whoever finds it there, rather than let it accumulate copies.
if [ "$attached" != "true" ]; then
  echo "Linear rejected the attachment keying this issue, trashing it" >&2

  reason='Filed by the flaky test report, which then failed to attach the key it finds this issue by.
Trashed so the next report can file a clean one. The test is still flaky.'

  api "$(jq -n --arg id "$issue_id" --arg body "$reason" '{
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
note "$issue_id" || exit 2
