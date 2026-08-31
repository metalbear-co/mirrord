#!/usr/bin/env bash
#
# Builds the Slack payload announcing tests that retried for the first time.
#
# Reads `<package>\t<test>\t<issue>\t<issue-url>` lines on stdin, where the last two may be empty,
# and writes the webhook payload to stdout. Each test is named by its trailing segments and linked to
# the Linear issue filed for it, so the channel needs no trip to the run to see what is flaking.
#
# Usage: flaky-notify.sh <repo> <run-url>

set -euo pipefail

repo=$1
run_url=$2

short=${repo#*/}
repo_url="https://github.com/$repo"
bullets=()

while IFS=$'\t' read -r package name issue issue_url; do
  [ -n "$name" ] || continue

  bullet="•  \`${package##*::}\`/\`${name##*::}\`"

  if [ -n "${issue:-}" ]; then
    bullet="$bullet  ·  <$issue_url|$issue>"
  fi

  bullets+=("$bullet")
done

if [ "${#bullets[@]}" -eq 0 ]; then
  echo "nothing to report" >&2
  exit 1
fi

list=$(printf '%s\n' "${bullets[@]}")

# A Slack section caps out at 3000 characters, which a thoroughly broken run can cross.
dropped=0
while [ "${#list}" -gt 2900 ] && [ "${#bullets[@]}" -gt 1 ]; do
  unset 'bullets[-1]'
  dropped=$((dropped + 1))
  list=$(printf '%s\n' "${bullets[@]}")
done

if [ "$dropped" -ne 0 ]; then
  list=$(printf '%s\n_…and %s more._' "$list" "$dropped")
fi

jq -n \
  --arg headline "Flaky tests detected on $short" \
  --arg list "$list" \
  --arg footer "<$repo_url|Repository> • <$run_url|Run>" \
  '{
    text: $headline,
    blocks: [
      {type: "header", text: {type: "plain_text", text: (":game_die: " + $headline), emoji: true}},
      {type: "section", text: {type: "mrkdwn", text: $list}},
      {type: "context", elements: [{type: "mrkdwn", text: $footer}]}
    ]
  }'
