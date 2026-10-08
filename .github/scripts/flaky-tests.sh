#!/usr/bin/env bash
#
# Reports the tests nextest had to retry in one `main` run, which a green run hides entirely.
#
# Reads the JUnit reports CI merges into that run's `nextest-junit` artifact, counting a test's
# `flakyFailure` elements (attempts that failed before it passed) and `rerunFailure` ones (attempts
# of a test that failed for good). A run that failed publishes the reports of the jobs it did get
# through, since a broken `main` retries tests like any other.
#
# Usage: flaky-tests.sh <repo> <run-id> <out-dir>
#
# Reports every test that was retried at all, leaving the threshold to whoever decides what is worth
# filing: a test already tracked by an open issue is noted on it however rarely it flakes. The cases
# rstest generates are reported as the test function that declares them, so a parametrized test is
# one test however many of its cases flaked.
#
# Writes a directory per test into <out-dir>, holding the captured failure output of its retried cases
# in `output.log` and, for a parametrized test, a markdown table of their arguments in `cases.md`. Writes
# a markdown table to stdout. On $GITHUB_OUTPUT it sets `count` and `tests`, the latter
# `<retries>\t<package>\t<test>\t<dir>` lines, most retried first.

set -euo pipefail

repo=$1
run_id=$2
out_dir=$3

readonly ARTIFACT=nextest-junit

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

mkdir -p "$work/reports" "$out_dir"

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

python3 - "$work/reports" "$work/ranked.tsv" "$out_dir" << 'PY'
import pathlib, re, sys
from collections import Counter
from xml.etree import ElementTree

reports, out, sightings = (pathlib.Path(arg) for arg in sys.argv[1:4])
retries = Counter()
failures = {}
names = {}

# rstest declares each case as a module under its test function: `case_<n>[_<description>]` for a
# `#[case]`, and `<argument>_<n>_<value>` for every `#[values]` argument, where <n> counts from one and
# <value> is the argument's expression with its punctuation folded into underscores. A plain test can
# be named that way too, so a name is read as a case only when everything the report holds under the
# same function is one.
CASE = re.compile(r"case_(\d+)(?:_(\w*))?")
VALUE = re.compile(r"(\w+?)_(\d+)_(\w*)")


def parameter(segment):
    if match := CASE.fullmatch(segment):
        return "case", match[1], match[2] or ""
    if match := VALUE.fullmatch(segment):
        return match.groups()
    return None


def function(pkg, test):
    segments = test.split("::")
    while len(segments) > 1 and parameter(segments[-1]):
        segments.pop()
    prefix = "::".join(segments) + "::"
    cases = (name[len(prefix):] for name in names[pkg] if name.startswith(prefix))
    if all(parameter(segment) for case in cases for segment in case.split("::")):
        return prefix.removesuffix("::")
    return test


def table(test, cases):
    rows = [[parameter(segment) for segment in case.removeprefix(f"{test}::").split("::")] for case in cases]
    lines = ["| " + " | ".join(f"`{name}`" for name, _, _ in rows[0]) + " |", "|" + "---|" * len(rows[0])]
    for row in rows:
        cells = (f"{int(n)} `{value}`" if value else str(int(n)) for _, n, value in row)
        lines.append("| " + " | ".join(cells) + " |")
    return "\n".join(lines) + "\n"


for report in reports.rglob("*.xml"):
    try:
        root = ElementTree.parse(report).getroot()
    except ElementTree.ParseError:
        continue

    for case in root.iter("testcase"):
        key = (case.get("classname", "?"), case.get("name", "?"))
        names.setdefault(key[0], set()).add(key[1])
        attempts = len(case.findall("flakyFailure")) + len(case.findall("rerunFailure"))
        if attempts:
            retries[key] += attempts
            output = failures.setdefault(key, [])
            for attempt in list(case):
                if attempt.tag not in ("failure", "error", "flakyFailure", "flakyError", "rerunFailure", "rerunError"):
                    continue
                output.append(f"--- {attempt.tag}: {attempt.get('message', '')} ---")
                if attempt.text and attempt.text.strip():
                    output.append(attempt.text.strip())
                for stream in ("system-out", "system-err"):
                    # Initial failures store output on the testcase; retries carry their own.
                    parent = case if attempt.tag in ("failure", "error") else attempt
                    for captured in parent.findall(stream):
                        if captured.text:
                            output.extend((f"--- {stream} ---", captured.text))

tests = {}
for (pkg, case), count in retries.items():
    tests.setdefault((pkg, function(pkg, case)), Counter())[case] = count

ranked = sorted(tests.items(), key=lambda kv: (-kv[1].total(), kv[0]))
rows = []
for index, ((pkg, test), cases) in enumerate(ranked):
    sighting = sightings / str(index)
    sighting.mkdir()
    sections = (f"=== {case} ===\n" + "\n".join(failures[(pkg, case)]) for case in sorted(cases))
    (sighting / "output.log").write_text("\n".join(sections) + "\n")
    # Prefix every captured line so test output cannot issue Actions workflow commands.
    for case in sorted(cases):
        print(f"Test failure: {pkg}/{case}")
        for line in "\n".join(failures[(pkg, case)]).splitlines():
            print(f"| {line}")
    (sighting / "cases.md").write_text("" if test in cases else table(test, sorted(cases)))
    rows.append(f"{cases.total()}\t{pkg}\t{test}\t{sighting}\n")
out.write_text("".join(rows))
PY

{
  echo "| retries | test |"
  echo "|---:|---|"
  awk -F'\t' '{printf "| %s | `%s`/`%s` |\n", $1, $2, $3}' "$work/ranked.tsv"
} | tee -a "${GITHUB_STEP_SUMMARY:-/dev/null}"

if [ -n "${GITHUB_OUTPUT:-}" ]; then
  {
    echo "count=$(awk 'END {print NR}' "$work/ranked.tsv")"
    echo "tests<<EOF"
    cat "$work/ranked.tsv"
    echo "EOF"
  } >> "$GITHUB_OUTPUT"
fi
