#!/usr/bin/env bash
#
# Deletes Actions artifacts that exist only to hand data between jobs of one run.
#
# Usage: prune-artifacts.sh run <run-id>    everything disposable in that run
#        prune-artifacts.sh stale <hours>   everything disposable older than <hours>, repo-wide
#
# `run` is the fast path CI takes when a run goes green. `stale` is the nightly backstop for the runs
# that path never reaches: failed and cancelled ones, and fork PRs, whose token cannot delete. Listing
# a whole repo walks every page of a five-figure collection, so `stale` takes minutes before it can
# delete anything.
#
# Set DRY_RUN to list what would go without deleting it.
#
# Reads $GITHUB_REPOSITORY. Needs `actions: write`, and reports rather than fails without it.

set -euo pipefail

mode=${1:-}
arg=${2:-}

# How many deletes to keep in flight. The REST API tolerates this comfortably; much more risks a
# secondary rate limit, which costs more than it saves.
readonly PARALLEL=8

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# Artifacts something reads after the run ends. Everything else goes, so an artifact meant to outlive
# its run belongs here. `nextest-junit` is the merged report flaky-tests.sh scans; the per-job
# `nextest-junit-*` it is merged from are disposable, and on a run that never merged them, no longer
# reachable by anything.
keep() {
  case "$1" in
    nextest-junit|intproxy_logs_*) return 0 ;;
    *) return 1 ;;
  esac
}

case "$mode" in
  run)
    echo "Listing artifacts of run $arg ..."
    gh api --paginate "repos/$GITHUB_REPOSITORY/actions/runs/$arg/artifacts?per_page=100" \
      > "$work/listing"
    filter='.artifacts[] | select(.expired | not)'
    ;;
  stale)
    cutoff=$(date -u -d "$arg hours ago" +%Y-%m-%dT%H:%M:%SZ)
    echo "Listing every artifact in $GITHUB_REPOSITORY (minutes, on a big repo) ..."
    gh api --paginate "repos/$GITHUB_REPOSITORY/actions/artifacts?per_page=100" > "$work/listing"
    filter=".artifacts[] | select(.expired | not) | select(.created_at < \"$cutoff\")"
    echo "Listed $(jq -rs '[.[].artifacts[]] | length' "$work/listing") artifacts," \
      "expired ones included. Of those live and older than $arg hours:"
    ;;
  *)
    echo "usage: $0 run <run-id> | stale <hours>" >&2
    exit 1
    ;;
esac

kept=0
while read -r id size name; do
  [ -n "$id" ] || continue

  if keep "$name"; then
    kept=$((kept + 1))
    continue
  fi

  printf '%s %s %s\n' "$id" "$size" "$name" >> "$work/doomed"
done < <(jq -r "$filter | \"\(.id) \(.size_in_bytes) \(.name)\"" "$work/listing")

touch "$work/doomed"
doomed=$(grep -c . "$work/doomed" || true)
bytes=$(awk '{s+=$2} END{print s+0}' "$work/doomed")

echo "$doomed to delete ($((bytes / 1024 / 1024))MB), $kept kept."

if [ "$doomed" -eq 0 ]; then
  exit 0
fi

if [ -n "${DRY_RUN:-}" ]; then
  cut -d' ' -f3- "$work/doomed" | sed 's/^/would delete /'
  echo "Dry run, nothing deleted."
  exit 0
fi

# shellcheck disable=SC2016 # $0 and $1 are the child shell's arguments, not this shell's.
cut -d' ' -f1,3- "$work/doomed" \
  | xargs -r -P "$PARALLEL" -n 2 sh -c '
      if error=$(gh api -X DELETE "repos/$GITHUB_REPOSITORY/actions/artifacts/$0" 2>&1); then
        echo "deleted $1"
      else
        echo "FAILED  $1: $(printf "%s" "$error" | tr "\n" " " | cut -c1-140)"
      fi
    ' \
  | tee "$work/results"

deleted=$(grep -c '^deleted ' "$work/results" || true)
failed=$(grep -c '^FAILED' "$work/results" || true)

echo "$deleted deleted, $failed failed, $kept kept."
