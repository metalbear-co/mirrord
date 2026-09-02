#!/usr/bin/env bash
#
# Measures a CI run against its budgets and builds the Slack alert for it.
#
# Splits the run's wall time into the part with at least one job running and the part with none,
# holding each to its budget and the wall time to the median of recent successful `main` runs. A run
# GitHub stopped at a job timeout gets its own alert naming the jobs it killed, which the budgets
# would otherwise never see.
#
# Usage: ci-duration.sh <repo> <run-id> <conclusion> <budget> <idle-budget> <median-runs>
#
# Writes the run's numbers to $GITHUB_STEP_SUMMARY, and the webhook payload to stdout when a budget
# was breached or the run was killed.

set -euo pipefail

repo=$1
run_id=$2
conclusion=$3
budget=$4
idle_budget=$5
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
whole_number 'the idle budget' "$idle_budget"
whole_number 'the sample size' "$median_runs"

# A page of runs tops out at 100, and sampling past that would report a median over more runs than
# it read.
if [ "$median_runs" -gt 100 ]; then
  echo "::warning::sample size $median_runs exceeds the 100-run page limit, sampling 100" >&2
  median_runs=100
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

run=$(gh api "repos/$repo/actions/runs/$run_id")
jobs=$(gh api --paginate "repos/$repo/actions/runs/$run_id/jobs?per_page=100" --jq '.jobs[]' | jq -s '.')

field() {
  jq -r "$1" <<< "$run"
}

minutes() {
  echo $(( ( $(date -d "$2" +%s) - $(date -d "$1" +%s) + 30 ) / 60 ))
}

total=$(minutes "$(field .run_started_at)" "$(field .updated_at)")

# Every figure covers a single attempt. A re-run resets `run_started_at` but not `created_at`, and a
# partial re-run carries the jobs that already passed into the new attempt with their original
# timestamps, so job intervals are clipped to the attempt's own window before anything is counted.
#
# Work is what a run spent with at least one job running, so overlapping jobs count once and the
# gaps between them do not. What is left over is the run sitting idle, waiting on runners: a job's
# own wait cannot stand in for it, since the rest of the matrix usually keeps working through it.
worked_minutes() {
  jq -r --arg from "$2" --arg to "$3" '
    ($from | fromdateiso8601) as $opened
    | ($to | fromdateiso8601) as $closed
    | [.[] | select(.started_at != null and .completed_at != null)
       | {s: ([(.started_at | fromdateiso8601), $opened] | max),
          e: ([(.completed_at | fromdateiso8601), $closed] | min)}
       | select(.e > .s)]
    | sort_by(.s)
    | reduce .[] as $job ([];
        if length > 0 and $job.s <= .[-1].e
        then .[0:-1] + [{s: .[-1].s, e: ([.[-1].e, $job.e] | max)}]
        else . + [$job]
        end)
    | map(.e - .s) | add // 0
    | . / 60 | round' <<< "$1"
}

run_jobs() {
  gh api --paginate "repos/$repo/actions/runs/$1/jobs?per_page=100" --jq '.jobs[]' | jq -s '.'
}

median_of() {
  sort -n | awk '{ v[NR] = $1 } END {
    if (NR == 0) { print 0 }
    else if (NR % 2) { print v[(NR + 1) / 2] }
    else { printf "%d\n", (v[NR / 2] + v[NR / 2 + 1]) / 2 + 0.5 }
  }'
}

execution=$(worked_minutes "$jobs" "$(field .run_started_at)" "$(field .updated_at)")
idle=$(( total - execution ))
[ "$idle" -ge 0 ] || idle=0

# How many sampled runs to read at once. The REST API tolerates this comfortably; much more risks
# a secondary rate limit, which costs more than it saves.
readonly PARALLEL=8

# One jobs request per sampled run, so this only runs once a budget has already been breached. A run
# whose jobs cannot be read is left out of the medians rather than taking the alert down with it.
sample_medians() {
  local id opened closed wall running=0 sample

  sample=$(gh api \
    "repos/$repo/actions/workflows/$(field .workflow_id)/runs?status=success&branch=main&per_page=$median_runs")

  while IFS=$'\t' read -r id opened closed wall; do
    [ -n "$id" ] || continue

    {
      if worked=$(worked_minutes "$(run_jobs "$id")" "$opened" "$closed" 2> /dev/null); then
        printf '%s\t%s\t%s\n' "$worked" "$(( wall > worked ? wall - worked : 0 ))" "$wall" \
          > "$work/$id.tsv"
      fi

      exit 0
    } &

    running=$((running + 1))

    if [ "$running" -ge "$PARALLEL" ]; then
      wait -n
      running=$((running - 1))
    fi
  done < <(jq -r '.workflow_runs[]
    | "\(.id)\t\(.run_started_at)\t\(.updated_at)\t\((((.updated_at | fromdateiso8601) - (.run_started_at | fromdateiso8601)) / 60) | round)"' <<< "$sample")

  wait
  cat "$work"/*.tsv 2> /dev/null || true
}

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
  local ran name

  while IFS=$'\t' read -r ran name; do
    [ -n "$name" ] || continue
    retained+=("$(printf '•  *%sm*  `%s`' "$ran" "$name")")
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
  printf 'CI run worked %sm (budget %sm), idled %sm (budget %sm), %sm in total.\n' \
    "$execution" "$budget" "$idle" "$idle_budget" "$total" >> "$GITHUB_STEP_SUMMARY"
fi

if [ -n "$killed" ]; then
  headline="$short CI was killed after ${total}m"
  facts=$(printf '•  *Execution*  %sm\n•  *Total*  %sm\n•  *Change*  %s' \
    "$execution" "$total" "$change")
  detail=$(printf '*Jobs GitHub stopped*\n%s' "$(bullets <<< "$killed")")
  footer='GitHub stops a job once it passes its `timeout-minutes`.'
  emoji=':skull:'
elif [ "$conclusion" != "success" ]; then
  exit 0
elif [ "$execution" -gt "$budget" ] || [ "$idle" -gt "$idle_budget" ]; then
  if [ "$execution" -gt "$budget" ] && [ "$idle" -gt "$idle_budget" ]; then
    headline="$short CI worked ${execution}m and idled ${idle}m"
    emoji=':stopwatch:'
  elif [ "$execution" -gt "$budget" ]; then
    headline="$short CI worked ${execution}m"
    emoji=':stopwatch:'
  else
    headline="$short CI idled ${idle}m waiting for runners"
    emoji=':hourglass:'
  fi

  sampled=$(sample_medians)

  facts=$(printf '•  *Execution*  %sm  ·  budget %sm  ·  median %sm\n•  *Idle*  %sm  ·  budget %sm  ·  median %sm\n•  *Total*  %sm  ·  median %sm\n•  *Change*  %s' \
    "$execution" "$budget" "$(cut -f1 <<< "$sampled" | median_of)" \
    "$idle" "$idle_budget" "$(cut -f2 <<< "$sampled" | median_of)" \
    "$total" "$(cut -f3 <<< "$sampled" | median_of)" "$change")
  detail=$(printf '*Slowest jobs*\n%s' \
    "$(job_lines '.[] | select(.started_at != null)' | sort -rn | head -3 | bullets)")
  footer="Execution counts the time at least one job was running; idle is the rest. Medians over the last $median_runs successful \`main\` runs."
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
