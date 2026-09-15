"""Link retried tests to their captured output in the completed collector job."""

import json
import os
import re
import subprocess
from pathlib import Path


def failure_links(log, job):
    step = next(
        step for step in job["steps"] if step["name"] == "Collect retried tests"
    )
    lines = log.splitlines()
    # Use the actual runner preamble rather than assuming a fixed number of command/env lines.
    start = next(
        i
        for i, line in enumerate(lines)
        if "##[group]Run .github/scripts/flaky-tests.sh" in line
    )
    debug_start = next(
        (
            i
            for i, line in enumerate(lines[:start])
            if "##[debug]Evaluating condition for step: 'Collect retried tests'" in line
        ),
        None,
    )
    if debug_start is not None:
        start = debug_start
    links = {}
    current = None
    found_panic = False
    found_output = False
    for index, line in enumerate(lines[start:], 1):
        content = re.sub(r"^\S+Z ", "", line)
        if content.startswith("Test failure: "):
            current = content.removeprefix("Test failure: ")
            found_panic = False
            found_output = False
            links[current] = f"{job['html_url']}#step:{step['number']}:{index}"
        elif current and content.startswith("| "):
            panic = "panicked at" in content
            if not found_output or (panic and not found_panic):
                links[current] = f"{job['html_url']}#step:{step['number']}:{index}"
            found_output = True
            found_panic = found_panic or panic
        elif not content.startswith("| "):
            current = None
    return links


def api(endpoint, *options):
    return subprocess.check_output(["gh", "api", endpoint, *options], text=True)


def main():
    links = {}
    try:
        repo = os.environ["GITHUB_REPOSITORY"]
        run = os.environ["GITHUB_RUN_ID"]
        attempt = os.environ["GITHUB_RUN_ATTEMPT"]
        pages = json.loads(
            api(
                f"repos/{repo}/actions/runs/{run}/attempts/{attempt}/jobs?per_page=100",
                "--paginate",
                "--slurp",
            )
        )
        job = next(
            job
            for page in pages
            for job in page["jobs"]
            if job["name"] == "Collect flaky failures"
        )
        links = failure_links(api(f"repos/{repo}/actions/jobs/{job['id']}/logs"), job)
    except (
        subprocess.CalledProcessError,
        ValueError,
        KeyError,
        StopIteration,
    ) as error:
        # Missing/expired logs must not prevent the existing issue and tally from being updated.
        print(f"::warning::Could not resolve failure log links: {error}")

    rows = []
    for row in os.environ["CI_FLAKY_TESTS"].splitlines():
        count, package, test = row.split("\t")
        rows.append(f"{count}\t{package}\t{test}\t{links.get(f'{package}/{test}', '')}")
    with Path(os.environ["GITHUB_OUTPUT"]).open("a") as output:
        output.write(
            "tests<<FLAKY_TESTS_END\n" + "\n".join(rows) + "\nFLAKY_TESTS_END\n"
        )


if __name__ == "__main__":
    main()
