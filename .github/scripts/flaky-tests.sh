#!/usr/bin/env bash
#
# Reports the tests nextest had to retry in one `main` run, which a green run hides entirely.
#
# Reads the JUnit reports CI merges into that run's `nextest-junit` artifact, counting a test's
# `flakyFailure` elements (attempts that failed before it passed) and `rerunFailure` ones (attempts
# of a test that failed for good). A run that failed publishes the reports of the jobs it did get
# through, since a broken `main` retries tests like any other.
#
# Usage: flaky-tests.sh <repo> <run-id>
#
# Reports every test that was retried at all, leaving the threshold to whoever decides what is worth
# filing: a test already tracked by an open issue is counted on it however rarely it flakes.
#
# Writes a markdown table to stdout. On $GITHUB_OUTPUT it sets `count` and `tests`, the latter
# `<retries>\t<package>\t<test>` lines, most retried first.

set -euo pipefail

repo=$1
run_id=$2

readonly ARTIFACT=nextest-junit

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

mkdir -p "$work/reports"

artifacts=$(gh api "repos/$repo/actions/runs/$run_id/artifacts?per_page=100")

artifact=$(jq -r --arg name "$ARTIFACT" '.artifacts[]? | select(.expired == false)
  | select(.name == $name) | .id' <<< "$artifacts" | awk 'NR == 1')

# Reporting zero flakes is right for a run that had no tests to retry and wrong for one whose report
# went missing, and the two are told apart by what is left behind: merging deletes the per-job
# reports as it goes, so any still there mean the merge never happened.
if [ -n "$artifact" ]; then
  if ! gh api "repos/$repo/actions/artifacts/$artifact/zip" > "$work/artifact.zip" 2> /dev/null ||
    ! unzip -qo "$work/artifact.zip" -d "$work/reports" 2> /dev/null; then
    echo "::error::could not read the $ARTIFACT artifact of run $run_id" >&2
    exit 1
  fi
else
  unmerged=$(jq -r --arg name "$ARTIFACT" '[.artifacts[]? | select(.name | startswith($name + "-"))]
    | length' <<< "$artifacts")

  if [ "$unmerged" -ne 0 ]; then
    echo "::error::run $run_id left $unmerged per-job report(s) unmerged" >&2
    exit 1
  fi

  echo "::warning::run $run_id published no $ARTIFACT artifact" >&2
fi

python3 - "$work/reports" "$work/ranked.tsv" <<'PY'
import pathlib, sys
from collections import Counter
from xml.etree import ElementTree

reports, out = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
retries = Counter()

for report in reports.rglob("*.xml"):
    try:
        root = ElementTree.parse(report).getroot()
    except ElementTree.ParseError:
        continue

    for case in root.iter("testcase"):
        attempts = len(case.findall("flakyFailure")) + len(case.findall("rerunFailure"))
        if attempts:
            retries[(case.get("classname", "?"), case.get("name", "?"))] += attempts

ranked = sorted(retries.items(), key=lambda kv: (-kv[1], kv[0]))
out.write_text("".join(f"{count}\t{pkg}\t{test}\n" for (pkg, test), count in ranked))
PY

echo "| retries | test |"
echo "|---:|---|"
awk -F'\t' '{printf "| %s | `%s`/`%s` |\n", $1, $2, $3}' "$work/ranked.tsv"

if [ -n "${GITHUB_OUTPUT:-}" ]; then
  {
    echo "count=$(awk 'END {print NR}' "$work/ranked.tsv")"
    echo "tests<<EOF"
    cat "$work/ranked.tsv"
    echo "EOF"
  } >> "$GITHUB_OUTPUT"
fi
