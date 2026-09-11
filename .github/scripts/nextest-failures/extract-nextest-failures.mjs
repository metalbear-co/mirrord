import { createHash } from 'node:crypto'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import path from 'node:path'
import { pathToFileURL } from 'node:url'

import { XMLParser } from 'fast-xml-parser'

const ARRAY_ELEMENTS = new Set([
  'error',
  'failure',
  'flakyError',
  'flakyFailure',
  'rerunError',
  'rerunFailure',
  'system-err',
  'system-out',
  'testcase',
  'testsuite',
])

const parser = new XMLParser({
  alwaysCreateTextNode: true,
  attributeNamePrefix: '',
  ignoreAttributes: false,
  isArray: (tagName, _jPath, _isLeafNode, isAttribute) =>
    isAttribute === false && ARRAY_ELEMENTS.has(tagName),
  parseAttributeValue: false,
  parseTagValue: false,
  trimValues: false,
})

function asArray(value) {
  if (value === undefined) {
    return []
  }

  return Array.isArray(value) ? value : [value]
}

function text(node) {
  if (node === undefined || node === null) {
    return ''
  }

  if (typeof node === 'string') {
    return node.trim()
  }

  return String(node['#text'] ?? '').trim()
}

function testcases(report) {
  const cases = []

  function visitSuite(suite) {
    cases.push(...asArray(suite.testcase))
    asArray(suite.testsuite).forEach(visitSuite)
  }

  asArray(report.testsuites?.testsuite).forEach(visitSuite)
  asArray(report.testsuite).forEach(visitSuite)

  return cases
}

function attemptOutput(attempt, stdout, stderr, index, total) {
  const lines = [`=== Attempt ${index} of ${total}: FAIL ===`]

  if (attempt.timestamp) {
    lines.push(`Timestamp: ${attempt.timestamp}`)
  }
  if (attempt.time) {
    lines.push(`Duration: ${attempt.time}s`)
  }
  if (attempt.type) {
    lines.push(`Type: ${attempt.type}`)
  }
  if (attempt.message) {
    lines.push(`Message: ${attempt.message}`)
  }

  const reason = text(attempt)
  if (reason && !stdout && !stderr) {
    lines.push('', '--- failure ---', reason)
  }
  if (stdout) {
    lines.push('', '--- stdout ---', stdout)
  }
  if (stderr) {
    lines.push('', '--- stderr ---', stderr)
  }

  return lines.join('\n')
}

function failureLog(testcase) {
  const failures = [...asArray(testcase.failure), ...asArray(testcase.error)]
  if (failures.length === 0) {
    return undefined
  }

  const retries = [
    ...asArray(testcase.rerunFailure),
    ...asArray(testcase.rerunError),
    ...asArray(testcase.flakyFailure),
    ...asArray(testcase.flakyError),
  ]
  const attempts = [...failures, ...retries]
  const directStdout = asArray(testcase['system-out'])
    .map(text)
    .filter(Boolean)
    .join('\n')
  const directStderr = asArray(testcase['system-err'])
    .map(text)
    .filter(Boolean)
    .join('\n')
  const output = attempts.map((attempt, index) => {
    const stdout =
      index === 0
        ? directStdout
        : asArray(attempt['system-out']).map(text).filter(Boolean).join('\n')
    const stderr =
      index === 0
        ? directStderr
        : asArray(attempt['system-err']).map(text).filter(Boolean).join('\n')

    return attemptOutput(attempt, stdout, stderr, index + 1, attempts.length)
  })
  const header = [
    `Test: ${testcase.name}`,
    `Binary: ${testcase.classname}`,
    `Failed attempts: ${attempts.length}`,
  ]

  return `${header.join('\n')}\n\n${output.join('\n\n')}\n`
}

function logFileName(testcase) {
  const identity = `${testcase.classname}\0${testcase.name}`
  const hash = createHash('sha256').update(identity).digest('hex').slice(0, 10)
  const readable = String(testcase.name ?? testcase.classname ?? 'unknown-test')
    .replaceAll('::', '__')
    .replace(/[^A-Za-z0-9._-]+/g, '_')
    .replace(/^[_\.]+|[_\.]+$/g, '')
    .slice(0, 180)

  return `${readable || 'unknown-test'}--${hash}.log`
}

export async function extractFailureLogs(reportPath, outputDirectory) {
  const xml = await readFile(reportPath, 'utf8')
  const report = parser.parse(xml)
  const failures = testcases(report)
    .map((testcase) => ({ log: failureLog(testcase), testcase }))
    .filter(({ log }) => log !== undefined)

  if (failures.length === 0) {
    return 0
  }

  await mkdir(outputDirectory, { recursive: true })
  await Promise.all(
    failures.map(({ log, testcase }) =>
      writeFile(path.join(outputDirectory, logFileName(testcase)), log, 'utf8'),
    ),
  )

  return failures.length
}

async function main() {
  const [reportPath, outputDirectory] = process.argv.slice(2)
  if (!reportPath || !outputDirectory) {
    throw new Error(
      'usage: extract-nextest-failures.mjs <junit.xml> <output-directory>',
    )
  }

  const count = await extractFailureLogs(reportPath, outputDirectory)
  console.log(`Wrote logs for ${count} failed test${count === 1 ? '' : 's'}.`)
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href
) {
  main().catch((error) => {
    console.error(error)
    process.exitCode = 1
  })
}
