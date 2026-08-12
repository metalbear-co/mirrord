#!/usr/bin/env bash
#
# Reports tests that nextest had to retry on `main`, which a green run hides entirely.
#
# Reads the JUnit reports each test job uploads, counting a test's `flakyFailure` elements (attempts
# that failed before it passed) and `rerunFailure` ones (attempts of a test that failed for good).
# Runs from before those artifacts existed contribute nothing.
#
# Usage: flaky-tests.sh <repo> <workflow-id> <runs-to-scan> <threshold> [urgent-threshold]
#
# Writes a markdown table to stdout. On $GITHUB_OUTPUT it sets `count` / `tests` for the report, and
# `urgent_count` / `urgent_threshold` / `urgent_tests` for tests at or above <urgent-threshold>,
# which are flaky enough to warrant their own issue. Both listings are `<retries>\t<test>` lines,
# most retried first.

set -euo pipefail

repo=$1
workflow=$2
runs=$3
threshold=$4
urgent_threshold=${5:-10}

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

mkdir -p "$work/reports"

gh api "repos/$repo/actions/workflows/$workflow/runs?per_page=$runs&status=success&branch=main" \
  --jq '.workflow_runs[].id' > "$work/runs.txt"

while read -r run; do
  gh api "repos/$repo/actions/runs/$run/artifacts?per_page=100" \
    --jq '.artifacts[]? | select(.expired == false) | select(.name | startswith("nextest-junit-")) | .id' 2>/dev/null |
    while read -r artifact; do
      if gh api "repos/$repo/actions/artifacts/$artifact/zip" > "$work/artifact.zip" 2>/dev/null; then
        unzip -qo "$work/artifact.zip" -d "$work/reports/$run-$artifact" 2>/dev/null || true
      fi
    done
done < "$work/runs.txt"

python3 - "$work/reports" "$threshold" "$work/ranked.tsv" <<'PY'
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

awk -F'\t' -v t="$threshold" '$1 >= t' "$work/ranked.tsv" > "$work/reported.tsv"
awk -F'\t' -v t="$urgent_threshold" '$1 >= t' "$work/ranked.tsv" > "$work/urgent.tsv"

echo "| retries | test |"
echo "|---:|---|"
awk -F'\t' '{printf "| %s | `%s` |\n", $1, $2}' "$work/reported.tsv"

if [ -n "${GITHUB_OUTPUT:-}" ]; then
  {
    echo "count=$(awk 'END {print NR}' "$work/reported.tsv")"
    echo "urgent_count=$(awk 'END {print NR}' "$work/urgent.tsv")"
    echo "urgent_threshold=$urgent_threshold"
    echo "tests<<EOF"
    cat "$work/reported.tsv"
    echo "EOF"
    echo "urgent_tests<<EOF"
    cat "$work/urgent.tsv"
    echo "EOF"
  } >> "$GITHUB_OUTPUT"
fi
