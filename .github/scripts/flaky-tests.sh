#!/usr/bin/env bash
#
# Reports tests that nextest had to retry on `main`, which a green run hides entirely.
#
# Reads the JUnit reports CI merges into one `nextest-junit` artifact per successful `main` run,
# counting a test's `flakyFailure` elements (attempts that failed before it passed) and
# `rerunFailure` ones (attempts of a test that failed for good).
#
# Sampling is by run that carried a report, never by run: a run that skipped the test matrix, or
# whose artifact has expired, is not one of the <samples> and does not consume one. Since only a
# successful `main` run publishes under that name, listing artifacts by it answers both questions at
# once, in a single request.
#
# Usage: flaky-tests.sh <repo> <samples> <notify-threshold> [issue-threshold]
#
# Writes a markdown table to stdout. On $GITHUB_OUTPUT it sets `scanned`, `notify_count` and
# `notify_tests` for the report, and `issue_count` / `issue_threshold` / `issue_tests` for tests at
# or above <issue-threshold>, which are flaky enough to warrant their own issue. Both listings are
# `<retries>\t<test>` lines, most retried first.

set -euo pipefail

repo=$1
samples=$2
notify_threshold=$3
issue_threshold=${4:-10}

readonly ARTIFACT=nextest-junit

whole_number() {
  case $2 in
    '' | *[!0-9]*)
      echo "$1 must be a whole number, got '$2'" >&2
      exit 1
      ;;
  esac
}

whole_number 'the sample size' "$samples"
whole_number 'the notify threshold' "$notify_threshold"
whole_number 'the issue threshold' "$issue_threshold"

# One page holds 100 artifacts, and asking for more would report on more runs than it read.
if [ "$samples" -gt 100 ]; then
  echo "::warning::sample size $samples exceeds the 100-artifact page limit, sampling 100" >&2
  samples=100
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

mkdir -p "$work/reports"

# Newest first, so taking the head of the listing takes the most recent runs. CI publishes this
# artifact on `main` alone; the branch is checked anyway so that widening that never widens this
# silently.
# Trimmed with awk rather than head, which would close the pipe on `gh` mid-write and take the
# script down with it under `pipefail`.
gh api "repos/$repo/actions/artifacts?per_page=100&name=$ARTIFACT" \
  --jq '.artifacts[]? | select(.expired == false) | select(.workflow_run.head_branch == "main")
        | "\(.id)\t\(.workflow_run.id)"' |
  awk -v samples="$samples" 'NR <= samples' > "$work/artifacts.tsv"

selected=$(awk 'END {print NR}' "$work/artifacts.tsv")

if [ "$selected" -lt "$samples" ]; then
  echo "::warning::asked for $samples runs, only $selected still carry a $ARTIFACT artifact" >&2
fi

# Counts the reports read, not the ones picked: one that fails to download or unpack takes its
# run's retries with it, and counting it would overstate the sample the alert reports on.
scanned=0

while IFS=$'\t' read -r artifact run; do
  [ -n "$artifact" ] || continue

  if gh api "repos/$repo/actions/artifacts/$artifact/zip" > "$work/artifact.zip" 2>/dev/null &&
    unzip -qo "$work/artifact.zip" -d "$work/reports/$run" 2>/dev/null; then
    scanned=$((scanned + 1))
  else
    echo "::warning::could not read the report of run $run" >&2
  fi
done < "$work/artifacts.tsv"

python3 - "$work/reports" "$notify_threshold" "$work/ranked.tsv" <<'PY'
import pathlib, sys
from collections import Counter
from xml.etree import ElementTree

reports, threshold, out = pathlib.Path(sys.argv[1]), int(sys.argv[2]), pathlib.Path(sys.argv[3])
retries = Counter()

for report in reports.rglob("*.xml"):
    try:
        root = ElementTree.parse(report).getroot()
    except ElementTree.ParseError:
        continue

    for case in root.iter("testcase"):
        attempts = len(case.findall("flakyFailure")) + len(case.findall("rerunFailure"))
        if attempts:
            retries[f"{case.get('classname', '?')} {case.get('name', '?')}"] += attempts

ranked = sorted(retries.items(), key=lambda kv: (-kv[1], kv[0]))
out.write_text("".join(f"{count}\t{test}\n" for test, count in ranked))
PY

awk -F'\t' -v t="$notify_threshold" '$1 >= t' "$work/ranked.tsv" > "$work/notify.tsv"
awk -F'\t' -v t="$issue_threshold" '$1 >= t' "$work/ranked.tsv" > "$work/issue.tsv"

echo "| retries | test |"
echo "|---:|---|"
awk -F'\t' '{printf "| %s | `%s` |\n", $1, $2}' "$work/notify.tsv"

if [ -n "${GITHUB_OUTPUT:-}" ]; then
  {
    echo "scanned=$scanned"
    echo "notify_count=$(awk 'END {print NR}' "$work/notify.tsv")"
    echo "issue_count=$(awk 'END {print NR}' "$work/issue.tsv")"
    echo "issue_threshold=$issue_threshold"
    echo "notify_tests<<EOF"
    cat "$work/notify.tsv"
    echo "EOF"
    echo "issue_tests<<EOF"
    cat "$work/issue.tsv"
    echo "EOF"
  } >> "$GITHUB_OUTPUT"
fi
