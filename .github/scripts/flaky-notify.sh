#!/usr/bin/env bash
#
# Builds the Slack payload announcing tests that retry on `main`.
#
# Reads the `<retries>\t<test>` listing `flaky-tests.sh` produces on stdin and writes the webhook
# payload to stdout. Every test is named in the message itself, annotated with the Linear issue
# tracking it when one is open, so the channel needs no trip to the run to see what is flaking.
#
# Usage: flaky-notify.sh <repo> <team-key> <notify-threshold> <runs-read>
#
# Annotates issues only when $LINEAR_APP_TOKEN is set.

set -euo pipefail

repo=$1
team_key=$2
notify_threshold=$3
runs=$4

scripts=$(dirname "$0")
bullets=()

while IFS=$'\t' read -r retries name; do
  [ -n "$name" ] || continue

  issue=""
  if [ -n "${LINEAR_APP_TOKEN:-}" ]; then
    if ! issue=$("$scripts/flaky-issue.sh" "$repo" "$team_key" "$name" < /dev/null); then
      echo "::warning::could not reach Linear for the issue tracking $name" >&2
      issue=""
    fi
  fi

  if [ -n "$issue" ]; then
    bullets+=("$(printf '•  *%s×*  `%s`  ·  %s' "$retries" "$name" "$issue")")
  else
    bullets+=("$(printf '•  *%s×*  `%s`' "$retries" "$name")")
  fi
done

count=${#bullets[@]}
if [ "$count" -eq 0 ]; then
  echo "nothing to report" >&2
  exit 1
fi

list=$(printf '%s\n' "${bullets[@]}")

# A Slack section caps out at 3000 characters, which a thoroughly broken `main` can cross.
dropped=0
while [ "${#list}" -gt 2900 ] && [ "${#bullets[@]}" -gt 1 ]; do
  unset 'bullets[-1]'
  dropped=$((dropped + 1))
  list=$(printf '%s\n' "${bullets[@]}")
done

if [ "$dropped" -ne 0 ]; then
  list=$(printf '%s\n_…and %s more._' "$list" "$dropped")
fi

if [ "$count" -eq 1 ]; then
  headline="1 flaky test on ${repo#*/} main"
else
  headline="$count flaky tests on ${repo#*/} main"
fi

jq -n \
  --arg headline "$headline" \
  --arg list "$list" \
  --arg footer "At least $notify_threshold retries across the last $runs \`main\` runs that published test reports." \
  '{
    text: $headline,
    blocks: [
      {type: "header", text: {type: "plain_text", text: (":game_die: " + $headline), emoji: true}},
      {type: "section", text: {type: "mrkdwn", text: $list}},
      {type: "context", elements: [{type: "mrkdwn", text: $footer}]}
    ]
  }'
