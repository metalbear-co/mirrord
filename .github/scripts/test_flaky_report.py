"""Offline regression tests: python3 .github/scripts/test_flaky_report.py"""

import json
import os
import subprocess
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
RUN_URL = "https://github.com/org/repo/actions/runs/999"
ASSET_URL = "https://uploads.linear.app/asset"
CASES = "| `arg` | `have` |\n|---|---|\n| 1 `true` | 1 `a` |\n| 1 `true` | 2 `b` |\n"
OPERATIONS = [
    "attachmentsForURL",
    "teams",
    "issueLabels",
    "issueCreate",
    "attachmentCreate",
    "fileUpload",
    "commentCreate",
    "issueDelete",
]

GH = """
import os, sys
from pathlib import Path
root = Path(os.environ['FIXTURES'])
endpoint = sys.argv[2]
if endpoint.endswith('/artifacts?per_page=100'):
    print('{"artifacts": [{"id": 7, "name": "nextest-junit", "expired": false}]}')
elif endpoint.endswith('/7/zip'):
    sys.stdout.buffer.write((root / 'reports.zip').read_bytes())
else:
    raise AssertionError(sys.argv)
"""

# Answers Linear's GraphQL API from the environment: $REPEAT has an open issue already track the
# test, $FAIL names the operations Linear answers with errors, and $REFUSE the ones it reports as
# unsuccessful.
CURL = """
import json, os, sys
from pathlib import Path
root = Path(os.environ['FIXTURES'])
args = sys.argv[1:]

def record(entry):
    with (root / 'requests').open('a') as output:
        output.write(json.dumps(entry) + '\\n')

if 'PUT' in args:
    record({
        'upload': args[args.index('PUT') + 1],
        'headers': [args[i + 1] for i, arg in enumerate(args) if arg == '-H'],
        'data': Path(next(arg for arg in args if arg.startswith('@'))[1:]).read_text(),
    })
    sys.exit(0)

request = json.loads(args[args.index('--data') + 1])
record(request)
query = request['query']
fail = os.environ.get('FAIL', '').split()
refuse = os.environ.get('REFUSE', '').split()
issue = {'id': 'issue', 'identifier': 'INT-1', 'url': 'https://linear.app/test'}
operation = next(name for name in %s if name + '(' in query)

if operation in fail:
    print(json.dumps({'errors': [{'message': 'rejected'}]}))
elif operation == 'attachmentsForURL':
    nodes = [{'issue': {**issue, 'state': {'type': 'started'}}}] if os.environ.get('REPEAT') else []
    print(json.dumps({'data': {operation: {'nodes': nodes}}}))
elif operation == 'teams':
    print(json.dumps({'data': {operation: {'nodes': [{'id': 'team'}]}}}))
elif operation == 'issueLabels':
    print(json.dumps({'data': {operation: {'nodes': []}}}))
elif operation == 'fileUpload':
    upload = {
        'uploadUrl': 'https://storage.test/upload',
        'assetUrl': '%s',
        'headers': [{'key': 'x-goog-meta', 'value': 'signed'}],
    }
    print(json.dumps({'data': {operation: {'success': True, 'uploadFile': upload}}}))
else:
    print(json.dumps({'data': {operation: {'success': operation not in refuse, 'issue': issue}}}))
""" % (OPERATIONS, ASSET_URL)


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
            "LINEAR_APP_TOKEN": "offline-test",
        }
        self.executable("gh", GH)
        self.executable("curl", CURL)
        (self.root / "requests").write_text("")
        self.sighting = self.root / "sighting"
        self.sighting.mkdir()
        self.log = self.sighting / "output.log"
        self.log.write_text("=== case ===\nthread 'case' panicked at file.rs:5\n")

    def executable(self, name, source):
        file = self.root / name
        file.write_text(f"#!{sys.executable}\n" + source)
        file.chmod(0o755)

    def run_script(self, name, *args, status=0):
        result = subprocess.run(
            ["bash", str(SCRIPTS / name), *args],
            env=self.env,
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(result.returncode, status, result.stderr)
        return result.stdout

    def file_issue(self, package, test, retries, threshold, cases="", status=0):
        (self.sighting / "cases.md").write_text(cases)
        return self.run_script(
            "flaky-issue.sh",
            "org/repo",
            "INT",
            package,
            test,
            str(retries),
            str(threshold),
            RUN_URL,
            str(self.sighting),
            status=status,
        )

    def requests(self):
        return [
            json.loads(line)
            for line in (self.root / "requests").read_text().splitlines()
        ]

    def operations(self):
        return [
            "upload"
            if "upload" in request
            else next(name for name in OPERATIONS if name + "(" in request["query"])
            for request in self.requests()
        ]

    def request(self, operation):
        return next(
            request
            for request in self.requests()
            if "query" in request and operation + "(" in request["query"]
        )

    def test_collector_groups_rstest_cases_under_their_test(self):
        with zipfile.ZipFile(self.root / "reports.zip", "w") as archive:
            archive.writestr(
                "first.xml",
                """<testsuites><testsuite>
              <testcase classname="pkg" name="plain"><flakyFailure message="retry">
                <system-out>before panic</system-out>
                <system-err>thread 'plain' panicked at file.rs:5\n::error::untrusted</system-err>
              </flakyFailure></testcase>
              <testcase classname="pkg::bin" name="fn::arg_1_true::have_1_a">
                <flakyFailure message="retry"><system-err>first case</system-err></flakyFailure>
              </testcase>
              <testcase classname="pkg::bin" name="fn::arg_1_true::have_2_b"><failure message="failed"/>
                <system-err>initial failure</system-err>
                <rerunFailure><system-err>retry failure</system-err></rerunFailure>
              </testcase>
              <testcase classname="pkg::bin" name="other::case_01_described"><flakyFailure/></testcase>
              <testcase classname="pkg::bin" name="other::case_02"><flakyFailure/></testcase>
              <testcase classname="pkg::bin" name="cor_1401_issue_named"><flakyFailure/></testcase>
              <testcase classname="pkg::bin" name="fn::arg_2_false::have_1_a"/>
              <testcase classname="pkg::bin" name="module::nested"><flakyFailure/></testcase>
              <testcase classname="pkg::bin" name="module::key_is_5_hex"><flakyFailure/></testcase>
              <testcase classname="pkg" name="passed"/>
            </testsuite></testsuites>""",
            )
            archive.writestr(
                "second.xml",
                '<testsuite><testcase classname="pkg" name="plain"><flakyFailure message="again"/></testcase></testsuite>',
            )

        sightings = self.root / "sightings"
        printed = self.run_script("flaky-tests.sh", "org/repo", "999", str(sightings))

        output = (self.root / "output").read_text()
        self.assertIn("count=6\n", output)
        self.assertIn(
            "tests<<EOF\n"
            f"2\tpkg\tplain\t{sightings}/0\n"
            f"2\tpkg::bin\tfn\t{sightings}/1\n"
            f"2\tpkg::bin\tother\t{sightings}/2\n"
            f"1\tpkg::bin\tcor_1401_issue_named\t{sightings}/3\n"
            f"1\tpkg::bin\tmodule::key_is_5_hex\t{sightings}/4\n"
            f"1\tpkg::bin\tmodule::nested\t{sightings}/5\n"
            "EOF\n",
            output,
        )

        self.assertEqual((sightings / "1" / "cases.md").read_text(), CASES)
        self.assertEqual(
            (sightings / "2" / "cases.md").read_text(),
            "| `case` |\n|---|\n| 1 `described` |\n| 2 |\n",
        )
        for plain in ["0", "3", "4", "5"]:
            self.assertEqual((sightings / plain / "cases.md").read_text(), "")

        plain = (sightings / "0" / "output.log").read_text()
        self.assertIn("=== plain ===", plain)
        self.assertIn("::error::untrusted", plain)

        parametrized = (sightings / "1" / "output.log").read_text()
        for expected in [
            "=== fn::arg_1_true::have_1_a ===",
            "first case",
            "=== fn::arg_1_true::have_2_b ===",
            "initial failure",
            "retry failure",
        ]:
            self.assertIn(expected, parametrized)

        self.assertIn("| 2 | `pkg::bin`/`fn` |", printed)
        self.assertIn("Test failure: pkg::bin/fn::arg_1_true::have_2_b\n", printed)
        self.assertIn("| ::error::untrusted\n", printed)
        self.assertNotIn("\n::error::untrusted", printed)
        self.assertNotIn("untrusted", (self.root / "summary").read_text())

    def test_new_parametrized_test_is_filed_once_and_noted(self):
        self.assertEqual(
            self.file_issue("pkg::bin", "fn", 2, 1, CASES),
            "new\tINT-1\thttps://linear.app/test\n",
        )
        self.assertEqual(
            self.operations(),
            [
                "attachmentsForURL",
                "teams",
                "issueLabels",
                "issueLabels",
                "issueCreate",
                "attachmentCreate",
                "fileUpload",
                "upload",
                "commentCreate",
            ],
        )

        key = "flaky-test://org/repo/pkg::bin/fn"
        self.assertEqual(self.request("attachmentsForURL")["variables"]["url"], key)

        created = self.request("issueCreate")["variables"]
        self.assertEqual(created["title"], "Fix flaky test `pkg`/`fn` in repo")
        self.assertIn(
            "cargo nextest run -p pkg -E 'binary_id(=pkg::bin) and test(/^fn::/)'",
            created["body"],
        )

        attached = self.request("attachmentCreate")["variables"]
        self.assertEqual(attached["url"], key)
        self.assertEqual(attached["metadata"]["test"], "fn")

        self.assertEqual(
            self.request("fileUpload")["variables"],
            {"type": "text/plain", "filename": "fn.log", "size": len(self.log.read_bytes())},
        )

        upload = next(request for request in self.requests() if "upload" in request)
        self.assertEqual(upload["upload"], "https://storage.test/upload")
        self.assertEqual(upload["data"], self.log.read_text())
        for header in [
            "Content-Type: text/plain",
            "Cache-Control: public, max-age=31536000",
            "x-goog-meta: signed",
        ]:
            self.assertIn(header, upload["headers"])

        self.assertEqual(
            self.request("commentCreate")["variables"]["body"],
            f"Failed in [this run]({RUN_URL}).\n\n{CASES}\n[fn.log]({ASSET_URL})",
        )

    def test_plain_test_is_run_alone_and_noted_without_cases(self):
        self.file_issue("pkg", "module::flaky", 1, 1)

        self.assertIn(
            "binary_id(=pkg) and test(=module::flaky)",
            self.request("issueCreate")["variables"]["body"],
        )
        self.assertEqual(
            self.request("commentCreate")["variables"]["body"],
            f"Failed in [this run]({RUN_URL}).\n\n[flaky.log]({ASSET_URL})",
        )

    def test_repeat_is_noted_however_rarely_it_flaked(self):
        self.env["REPEAT"] = "1"

        self.assertEqual(
            self.file_issue("pkg", "flaky", 1, 5),
            "repeat\tINT-1\thttps://linear.app/test\n",
        )
        self.assertEqual(
            self.operations(),
            ["attachmentsForURL", "fileUpload", "upload", "commentCreate"],
        )

    def test_sighting_that_cannot_be_noted_still_names_its_issue(self):
        self.env["FAIL"] = "commentCreate"

        for repeat, state in [("", "new"), ("1", "repeat")]:
            with self.subTest(state=state):
                self.env["REPEAT"] = repeat
                self.assertEqual(
                    self.file_issue("pkg", "flaky", 1, 1, status=2),
                    f"{state}\tINT-1\thttps://linear.app/test\n",
                )

    def test_sighting_is_noted_without_output_that_failed_to_upload(self):
        self.env["FAIL"] = "fileUpload"

        self.assertTrue(self.file_issue("pkg", "flaky", 1, 1).startswith("new\t"))
        self.assertNotIn("upload", self.operations())
        self.assertEqual(
            self.request("commentCreate")["variables"]["body"],
            f"Failed in [this run]({RUN_URL}).",
        )

    def test_rare_flake_is_not_filed(self):
        self.assertEqual(self.file_issue("pkg", "flaky", 1, 2), "below\t\t\n")
        self.assertEqual(self.operations(), ["attachmentsForURL"])

    def test_unkeyed_issue_is_trashed(self):
        for rejection, other in [("FAIL", "REFUSE"), ("REFUSE", "FAIL")]:
            with self.subTest(rejection=rejection):
                self.env.update({rejection: "attachmentCreate", other: ""})
                (self.root / "requests").write_text("")

                self.file_issue("pkg", "flaky", 1, 1, status=1)
                self.assertEqual(self.operations()[-2:], ["commentCreate", "issueDelete"])
                self.assertNotIn("fileUpload", self.operations())


if __name__ == "__main__":
    unittest.main()
