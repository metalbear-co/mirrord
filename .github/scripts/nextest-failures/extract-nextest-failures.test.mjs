import assert from 'node:assert/strict'
import { mkdtemp, readFile, readdir, writeFile } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import test from 'node:test'

import { extractFailureLogs } from './extract-nextest-failures.mjs'

test('writes one readable log per ultimately failed test', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'nextest-failures-'))
  const report = path.join(directory, 'junit.xml')
  const output = path.join(directory, 'failures')
  await writeFile(
    report,
    `<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="nextest-run" tests="4" failures="1" errors="1">
  <testsuite name="tests" tests="4" failures="1" errors="1">
    <testcase name="mirrord::passes" classname="tests" time="1.000"></testcase>
    <testcase name="mirrord::flaky" classname="tests" time="2.000">
      <flakyFailure message="first attempt failed" type="test failure">flaky reason</flakyFailure>
    </testcase>
    <testcase name="mirrord::cannot_execute" classname="tests" time="0.000">
      <error message="process did not start" type="exec failed">spawn error</error>
    </testcase>
    <testcase name="mirrord::fails/with unsafe chars" classname="tests" time="3.000">
      <failure timestamp="2026-08-13T17:25:21Z" time="1.000" message="expected &lt;ready&gt; &amp; got pending" type="test failure">initial reason</failure>
      <rerunFailure timestamp="2026-08-13T17:25:22Z" time="2.000" message="retry failed" type="test failure">
        retry reason
        <system-out>retry stdout</system-out>
        <system-err>retry stderr</system-err>
      </rerunFailure>
      <system-out>initial stdout</system-out>
      <system-err>initial stderr</system-err>
    </testcase>
  </testsuite>
</testsuites>`,
    'utf8',
  )

  assert.equal(await extractFailureLogs(report, output), 2)

  const files = await readdir(output)
  assert.equal(files.length, 2)
  const failedTestFile = files.find((file) => file.startsWith('mirrord__fails'))
  const execFailureFile = files.find((file) =>
    file.startsWith('mirrord__cannot_execute'),
  )
  assert.match(
    failedTestFile,
    /^mirrord__fails_with_unsafe_chars--[a-f0-9]{10}\.log$/,
  )
  assert.match(execFailureFile, /^mirrord__cannot_execute--[a-f0-9]{10}\.log$/)

  const log = await readFile(path.join(output, failedTestFile), 'utf8')
  assert.match(log, /Test: mirrord::fails\/with unsafe chars/)
  assert.match(log, /Failed attempts: 2/)
  assert.match(log, /=== Attempt 1 of 2: FAIL ===/)
  assert.match(log, /Message: expected <ready> & got pending/)
  assert.match(log, /initial stdout/)
  assert.match(log, /initial stderr/)
  assert.match(log, /=== Attempt 2 of 2: FAIL ===/)
  assert.match(log, /retry stdout/)
  assert.match(log, /retry stderr/)

  const execFailureLog = await readFile(
    path.join(output, execFailureFile),
    'utf8',
  )
  assert.match(execFailureLog, /Type: exec failed/)
  assert.match(execFailureLog, /--- failure ---\nspawn error/)
})

test('does not create an output directory when every test ultimately passes', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'nextest-failures-'))
  const report = path.join(directory, 'junit.xml')
  const output = path.join(directory, 'failures')
  await writeFile(
    report,
    `<testsuites><testsuite><testcase name="passes" classname="tests" /></testsuite></testsuites>`,
    'utf8',
  )

  assert.equal(await extractFailureLogs(report, output), 0)
  await assert.rejects(readdir(output), { code: 'ENOENT' })
})
