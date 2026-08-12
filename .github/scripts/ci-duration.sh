#!/usr/bin/env bash
#
# Measures a CI run against its budgets and builds the Slack alert for it.
#
# Splits the run into the time its jobs spent executing, the longest any one job waited for a
# runner, and the wall time between queueing and finishing, holding the first two to their budgets
# and both to the median of recent successful `main` runs. A run GitHub stopped at a job timeout
# gets its own alert naming the jobs it killed, which the budgets would otherwise never see.
#
# Usage: ci-duration.sh <repo> <run-id> <conclusion> <budget> <queue-budget> <median-runs>
#
# Writes the run's numbers to $GITHUB_STEP_SUMMARY, and the webhook payload to stdout when a budget
# was breached or the run was killed.

set -euo pipefail

repo=$1
run_id=$2
conclusion=$3
budget=$4
queue_budget=$5
median_runs=$6

short=${repo#*/}

whole_number() {
  case $2 in
    '' | *[!0-9]*)
      echo "$1 must be a whole number, got '$2'" >&2
      exit 1
      ;;
  esac
}

whole_number 'the execution budget' "$budget"
whole_number 'the queue budget' "$queue_budget"
whole_number 'the sample size' "$median_runs"

# A page of runs tops out at 100, and sampling past that would report a median over more runs than
# it read.
if [ "$median_runs" -gt 100 ]; then
  echo "::warning::sample size $median_runs exceeds the 100-run page limit, sampling 100" >&2
  median_runs=100
fi

run=$(gh api "repos/$repo/actions/runs/$run_id")
jobs=$(gh api --paginate "repos/$repo/actions/runs/$run_id/jobs?per_page=100" --jq '.jobs[]' | jq -s '.')

field() {
  jq -r "$1" <<< "$run"
}

minutes() {
  echo $(( ( $(date -d "$2" +%s) - $(date -d "$1" +%s) + 30 ) / 60 ))
}

# A run's own clock starts once GitHub picks it up, so the wait for a runner is only visible per
# job, and the one job that waited longest is what held the run back.
execution=$(minutes "$(field .run_started_at)" "$(field .updated_at)")
total=$(minutes "$(field .created_at)" "$(field .updated_at)")
queued=$(jq -r '
  [.[] | select(.created_at != null and .started_at != null)
   | (.started_at | fromdateiso8601) - (.created_at | fromdateiso8601)]
  | (max // 0) / 60 | round' <<< "$jobs")

median=$(gh api \
  "repos/$repo/actions/workflows/$(field .workflow_id)/runs?status=success&branch=main&per_page=$median_runs" \
  --jq '
    [.workflow_runs[]
     | ((.updated_at | fromdateiso8601) - (.run_started_at | fromdateiso8601)) / 60]
    | sort
    | length as $n
    | if $n == 0 then 0
      elif $n % 2 == 1 then .[$n / 2 | floor]
      else (.[$n / 2 - 1 | floor] + .[$n / 2 | floor]) / 2
      end
    | round')

pr=$(gh api "repos/$repo/commits/$(field .head_sha)/pulls" --jq '.[0].number // empty' || true)

if [ -n "$pr" ]; then
  change="<https://github.com/$repo/pull/$pr|$short#$pr>"
else
  sha=$(field .head_sha)
  change="<https://github.com/$repo/commit/$sha|\`${sha:0:7}\`>"
fi

job_lines() {
  jq -r "$1"' | "\(((( .completed_at // .started_at) | fromdateiso8601) - (.started_at | fromdateiso8601)) / 60 | round)\t\(.name)"' <<< "$jobs"
}

bullets() {
  local retained=()
  local retries name

  while IFS=$'\t' read -r retries name; do
    [ -n "$name" ] || continue
    retained+=("$(printf '•  *%sm*  `%s`' "$retries" "$name")")
  done

  [ "${#retained[@]}" -eq 0 ] || printf '%s\n' "${retained[@]}"
}

# GitHub cancels a job that passes its `timeout-minutes`, which reads exactly like a job a human or
# a concurrency rule stopped until you find the annotation it leaves behind.
timed_out_jobs() {
  local id name started completed messages

  while IFS=$'\t' read -r id name started completed; do
    [ -n "$id" ] || continue

    messages=$(gh api --paginate "repos/$repo/check-runs/$id/annotations" --jq '.[].message')

    case $messages in
      *'exceeded the maximum execution time'*)
        printf '%s\t%s\n' "$(minutes "$started" "$completed")" "$name"
        ;;
    esac
  done < <(jq -r '.[]
    | select(.conclusion == "cancelled") | select(.started_at != null)
    | "\(.id)\t\(.name)\t\(.started_at)\t\(.completed_at // .started_at)"' <<< "$jobs")
}

killed=$(timed_out_jobs)

if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  printf 'CI run took %sm executing (budget %sm, median %sm), waited %sm for a runner (budget %sm), %sm in total.\n' \
    "$execution" "$budget" "$median" "$queued" "$queue_budget" "$total" >> "$GITHUB_STEP_SUMMARY"
fi

if [ -n "$killed" ]; then
  headline="$short CI was killed after ${total}m"
  facts=$(printf '•  *Execution*  %sm  ·  median %sm\n•  *Total*  %sm\n•  *Change*  %s' \
    "$execution" "$median" "$total" "$change")
  detail=$(printf '*Jobs GitHub stopped*\n%s' "$(bullets <<< "$killed")")
  footer='GitHub stops a job once it passes its `timeout-minutes`.'
  emoji=':skull:'
elif [ "$conclusion" != "success" ]; then
  exit 0
elif [ "$execution" -gt "$budget" ] || [ "$queued" -gt "$queue_budget" ]; then
  if [ "$execution" -gt "$budget" ] && [ "$queued" -gt "$queue_budget" ]; then
    headline="$short CI took ${execution}m and waited ${queued}m for a runner"
    emoji=':stopwatch:'
  elif [ "$execution" -gt "$budget" ]; then
    headline="$short CI took ${execution}m"
    emoji=':stopwatch:'
  else
    headline="$short CI waited ${queued}m for a runner"
    emoji=':hourglass:'
  fi

  facts=$(printf '•  *Execution*  %sm  ·  budget %sm  ·  median %sm\n•  *Queued*  %sm  ·  budget %sm\n•  *Total*  %sm\n•  *Change*  %s' \
    "$execution" "$budget" "$median" "$queued" "$queue_budget" "$total" "$change")
  detail=$(printf '*Slowest jobs*\n%s' \
    "$(job_lines '.[] | select(.started_at != null)' | sort -rn | head -3 | bullets)")
  footer="Execution excludes the wait for a runner. Median over the last $median_runs successful \`main\` runs."
else
  exit 0
fi

jq -n \
  --arg headline "$headline" \
  --arg emoji "$emoji" \
  --arg facts "$facts" \
  --arg detail "$detail" \
  --arg footer "$footer" \
  '{
    text: $headline,
    blocks: [
      {type: "header", text: {type: "plain_text", text: ($emoji + " " + $headline), emoji: true}},
      {type: "section", text: {type: "mrkdwn", text: $facts}},
      {type: "section", text: {type: "mrkdwn", text: $detail}},
      {type: "context", elements: [{type: "mrkdwn", text: $footer}]}
    ]
  }'
