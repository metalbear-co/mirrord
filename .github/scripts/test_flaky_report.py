"""Offline regression tests: python3 .github/scripts/test_flaky_report.py"""

import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("flaky_links", SCRIPTS / "flaky-links.py")
links = importlib.util.module_from_spec(spec)
spec.loader.exec_module(links)


class FlakyReportTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.env = {
            **os.environ,
            "PATH": f"{self.root}:{os.environ['PATH']}",
            "FIXTURES": str(self.root),
            "GITHUB_OUTPUT": str(self.root / "output"),
            "GITHUB_STEP_SUMMARY": str(self.root / "summary"),
            "GITHUB_REPOSITORY": "org/repo",
            "GITHUB_RUN_ID": "123",
            "GITHUB_RUN_ATTEMPT": "2",
            "LINEAR_APP_TOKEN": "offline-test",
        }
        self.job = {
            "id": 456,
            "name": "Collect flaky failures",
            "html_url": "https://github.com/org/repo/actions/runs/123/job/456",
            "steps": [{"name": "Collect retried tests", "number": 3}],
        }
        (self.root / "jobs.json").write_text(
            json.dumps([{"jobs": []}, {"jobs": [self.job]}])
        )
        self.executable(
            "gh",
            """
import json, os, sys
from pathlib import Path
root = Path(os.environ['FIXTURES'])
endpoint = sys.argv[2]
if endpoint.endswith('/artifacts?per_page=100'):
    print(json.dumps({'artifacts': [{'id': 7, 'name': 'nextest-junit', 'expired': False}]}))
elif endpoint.endswith('/7/zip'):
    sys.stdout.buffer.write((root / 'reports.zip').read_bytes())
elif '/attempts/2/jobs?' in endpoint:
    print((root / 'jobs.json').read_text())
elif endpoint.endswith('/456/logs'):
    if os.environ.get('MISSING_LOGS'): sys.exit(1)
    # Real `gh` refuses a response carrying escape sequences unless asked, and a job log always
    # carries the runner's own colouring, so a fake that always answers hides a broken fetch.
    if '--allow-escape-sequences' not in sys.argv:
        print('the response contains terminal escape sequences; pass '
              '--allow-escape-sequences to output it anyway', file=sys.stderr)
        sys.exit(1)
    print((root / 'job.log').read_text(), end='')
else:
    raise AssertionError(sys.argv)
""",
        )

    def executable(self, name, source):
        file = self.root / name
        file.write_text(f"#!{sys.executable}\n" + source)
        file.chmod(0o755)

    def run_script(self, name, *args):
        command = [
            sys.executable if name.endswith(".py") else "bash",
            str(SCRIPTS / name),
            *args,
        ]
        result = subprocess.run(
            command, env=self.env, capture_output=True, text=True, check=False
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        return result.stdout

    def collect(self):
        with zipfile.ZipFile(self.root / "reports.zip", "w") as archive:
            archive.writestr(
                "first.xml",
                """<testsuites><testsuite>
              <testcase classname="pkg" name="flaky"><flakyFailure message="retry">
                <system-out>before panic</system-out>
                <system-err>thread 'flaky' panicked at file.rs:5\n::error::untrusted</system-err>
              </flakyFailure></testcase>
              <testcase classname="other" name="flaky"><failure message="failed"/>
                <system-err>initial failure</system-err>
                <rerunFailure><system-err>retry failure</system-err></rerunFailure>
              </testcase>
              <testcase classname="pkg" name="passed"/>
            </testsuite></testsuites>""",
            )
            archive.writestr(
                "second.xml",
                '<testsuite><testcase classname="pkg" name="flaky"><flakyFailure message="again"/></testcase></testsuite>',
            )
        return self.run_script("flaky-tests.sh", "org/repo", "999")

    def test_collector_and_links(self):
        output = self.collect()
        self.assertIn("| ::error::untrusted", output)
        self.assertIn("| initial failure", output)
        self.assertIn("| retry failure", output)
        self.assertNotIn("Test failure: pkg/passed", output)
        self.assertIn("2\tpkg\tflaky", (self.root / "output").read_text())
        self.assertNotIn("panicked", (self.root / "summary").read_text())
        preamble = [
            "##[group]Run .github/scripts/flaky-tests.sh",
            "\x1b[36;1mcommand\x1b[0m",
            "env:",
            "  RUN_ID: 999",
            "##[endgroup]",
        ]
        step_lines = preamble + output.splitlines()
        log = "2026-09-15T01:00:00.000Z checkout\n" + "".join(
            f"2026-09-15T01:00:01.000Z {line}\n" for line in step_lines
        )
        (self.root / "job.log").write_text(log)
        self.env["CI_FLAKY_TESTS"] = "2\tpkg\tflaky\n1\tother\tflaky"
        (self.root / "output").unlink()
        self.run_script("flaky-links.py")
        result = (self.root / "output").read_text()
        panic_line = next(
            i for i, line in enumerate(step_lines, 1) if "panicked at" in line
        )
        self.assertIn(f"{self.job['html_url']}#step:3:{panic_line}", result)
        other_line = step_lines.index("Test failure: other/flaky") + 2
        self.assertIn(f"{self.job['html_url']}#step:3:{other_line}", result)
        debug = "2026-09-15T01:00:00Z ##[debug]Evaluating condition for step: 'Collect retried tests'\n"
        direct_log = log.split("\n", 1)[1]
        self.assertEqual(
            links.failure_links(debug + direct_log, self.job)["pkg/flaky"],
            f"{self.job['html_url']}#step:3:{panic_line + 1}",
        )

    def test_logs_are_requested_verbatim(self):
        """`gh` refuses escape sequences unless asked, and every job log carries them."""
        self.env["CI_FLAKY_TESTS"] = "2\tpkg\tflaky"
        (self.root / "job.log").write_text(
            "2026-09-15T01:00:00.000Z ##[group]Run .github/scripts/flaky-tests.sh\n"
            "2026-09-15T01:00:01.000Z Test failure: pkg/flaky\n"
            "2026-09-15T01:00:02.000Z | \x1b[31mthread 'flaky' panicked at file.rs:5\x1b[0m\n"
        )
        self.run_script("flaky-links.py")
        self.assertIn(
            f"{self.job['html_url']}#step:3:3", (self.root / "output").read_text()
        )

    def test_repeat_keeps_the_last_known_link(self):
        """A run that resolved no link must not strip the one the issue already carries."""
        stored = self.job["html_url"] + "#step:3:42"
        self.executable(
            "curl",
            """
import json, os, sys
from pathlib import Path
root = Path(os.environ['FIXTURES'])
request = json.loads(sys.argv[sys.argv.index('--data') + 1])
with (root / 'requests').open('a') as output: output.write(json.dumps(request) + '\\n')
query = request['query']
if 'attachmentsForURL' in query:
    stored = {'occurrences':3, 'failureUrl': os.environ['STORED_FAILURE_URL']}
    data = {'attachmentsForURL': {'nodes':[{'id':'key', 'metadata':stored, 'issue':{'id':'issue', 'identifier':'INT-1', 'url':'https://linear.app/test', 'state':{'type':'started'}}}]}}
else:
    operation = next(name for name in ['issueUpdate','attachmentUpdate','commentCreate'] if name + '(' in query)
    data = {operation:{'success':True}}
print(json.dumps({'data':data}))
""",
        )
        self.env["STORED_FAILURE_URL"] = stored
        (self.root / "requests").write_text("")
        self.run_script(
            "flaky-issue.sh", "org/repo", "INT", "pkg", "flaky", "2", "1",
            "https://github.com/org/repo/actions/runs/999", "",
        )
        requests = [
            json.loads(line)
            for line in (self.root / "requests").read_text().splitlines()
        ]
        bodies = [r["variables"]["body"] for r in requests if "body" in r["variables"]]
        self.assertEqual(len(bodies), 2)
        self.assertIn(f"[Captured failure output]({stored})", bodies[0])
        self.assertNotIn("Captured failure output", bodies[1])
        written = next(
            r["variables"]["metadata"] for r in requests if "metadata" in r["variables"]
        )
        self.assertEqual(written["failureUrl"], stored)

    def test_missing_logs_preserve_tests(self):
        self.env.update(CI_FLAKY_TESTS="2\tpkg\tflaky", MISSING_LOGS="1")
        self.assertIn("::warning::", self.run_script("flaky-links.py"))
        self.assertIn("2\tpkg\tflaky\t\n", (self.root / "output").read_text())

    def test_issue_creation_and_repeat_links(self):
        self.executable(
            "curl",
            """
import json, os, sys
from pathlib import Path
root = Path(os.environ['FIXTURES'])
request = json.loads(sys.argv[sys.argv.index('--data') + 1])
with (root / 'requests').open('a') as output: output.write(json.dumps(request) + '\\n')
query = request['query']
if 'attachmentsForURL' in query:
    stored = {'occurrences':3}
    if os.environ.get('STORED_FAILURE_URL'): stored['failureUrl'] = os.environ['STORED_FAILURE_URL']
    nodes = [{'id':'key', 'metadata':stored, 'issue':{'id':'issue', 'identifier':'INT-1', 'url':'https://linear.app/test', 'state':{'type':'started'}}}] if os.environ.get('REPEAT') else []
    data = {'attachmentsForURL': {'nodes':nodes}}
elif 'teams(' in query: data = {'teams':{'nodes':[{'id':'team'}]}}
elif 'issueLabels(' in query: data = {'issueLabels':{'nodes':[]}}
else:
    operation = next(name for name in ['issueCreate','issueUpdate','attachmentCreate','attachmentUpdate','commentCreate'] if name + '(' in query)
    data = {operation:{'success':True, 'issue':{'id':'issue','identifier':'INT-1','url':'https://linear.app/test'}}}
print(json.dumps({'data':data}))
""",
        )
        url = self.job["html_url"] + "#step:3:42"
        for repeat in (False, True):
            for failure_url in (url, ""):
                with self.subTest(repeat=repeat, failure_url=failure_url):
                    self.env["REPEAT"] = "1" if repeat else ""
                    (self.root / "requests").write_text("")
                    self.run_script(
                        "flaky-issue.sh",
                        "org/repo",
                        "INT",
                        "pkg",
                        "flaky",
                        "2",
                        "1",
                        "https://github.com/org/repo/actions/runs/999",
                        failure_url,
                    )
                    requests = [
                        json.loads(line)
                        for line in (self.root / "requests").read_text().splitlines()
                    ]
                    bodies = [
                        r["variables"]["body"]
                        for r in requests
                        if "body" in r["variables"]
                    ]
                    self.assertEqual(len(bodies), 2 if repeat else 1)
                    for body in bodies:
                        if failure_url:
                            self.assertIn(f"[Captured failure output]({url})", body)
                        else:
                            self.assertNotIn("Captured failure output", body)
                    self.assertIn("Failed 5×" if repeat else "Failed 2×", bodies[0])


if __name__ == "__main__":
    unittest.main()
