const assert = require('node:assert/strict')
const fs = require('node:fs')
const os = require('node:os')
const path = require('node:path')
const { execFileSync, spawnSync } = require('node:child_process')
const { test } = require('node:test')
const {
  transition,
  readState,
  pruneState,
  notify,
} = require('./release-monitor.cjs')

function results(status = 'healthy') {
  return {
    check_latest_release: {
      result: status === 'assets' ? 'failure' : 'success',
      outputs: {
        tag: '1.2.3',
        missing_optional: status === 'warning' ? 'mirrord.exe' : '',
      },
    },
    test_install_paths: {
      result:
        status === 'install'
          ? 'failure'
          : status === 'assets'
            ? 'skipped'
            : 'success',
    },
    check_version_endpoint: {
      result: status === 'version' ? 'failure' : 'success',
    },
  }
}

const context = {
  repo: { owner: 'owner', repo: 'repo' },
  runId: 123,
  serverUrl: 'https://github.com',
  payload: { repository: { default_branch: 'main' } },
}

function mockGithub(artifacts = [], archive) {
  return {
    rest: {
      actions: {
        getWorkflowRun: async () => ({ data: { workflow_id: 42 } }),
        listArtifactsForRepo: () => {},
        downloadArtifact: async () => ({ data: archive }),
      },
    },
    paginate: {
      iterator: async function* () {
        yield { data: artifacts }
      },
    },
  }
}

test('alerts once across changing failures and release tags, recovers, then alerts on recurrence', () => {
  assert.deepEqual(transition(null, results()), {
    status: 'healthy',
    notify: false,
  })
  let previous = { status: 'healthy' }
  for (const [status, expected] of [
    ['assets', true],
    ['install', false],
    ['version', false],
    ['healthy', true],
    ['healthy', false],
    ['install', true],
  ]) {
    const checks = results(status)
    checks.check_latest_release.outputs.tag = status
    const next = transition(previous, checks)
    assert.equal(next.notify, expected, status)
    previous = next
  }
})

test('optional warnings alert once, escalate once, and recover only when fully healthy', () => {
  let previous = null
  for (const [status, expected] of [
    ['warning', true],
    ['warning', false],
    ['assets', true],
    ['warning', false],
    ['install', false],
    ['healthy', true],
  ]) {
    const next = transition(previous, results(status))
    assert.equal(next.notify, expected, status)
    previous = next
  }
})

test('cancelled or skipped checks cannot announce recovery', () => {
  for (const result of ['cancelled', 'skipped']) {
    const checks = results()
    checks.test_install_paths.result = result
    assert.deepEqual(transition({ status: 'failure' }, checks), {
      status: 'failure',
      notify: false,
    })
  }
})

test('restores persisted state, skipping expired and non-default-branch artifacts', async () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'monitor-test-'))
  try {
    fs.writeFileSync(
      path.join(directory, 'release-monitor-state.json'),
      JSON.stringify({ status: 'failure' }),
    )
    execFileSync('zip', ['-q', 'state.zip', 'release-monitor-state.json'], {
      cwd: directory,
    })
    const github = mockGithub(
      [
        { expired: true },
        { workflow_run: { head_branch: 'feature' } },
        { id: 1, workflow_run: { id: 122, head_branch: 'main' } },
      ],
      fs.readFileSync(path.join(directory, 'state.zip')),
    )
    assert.deepEqual(await readState({ github, context }), {
      status: 'failure',
    })
    github.rest.actions.downloadArtifact = async () => {
      throw new Error('API unavailable')
    }
    await assert.rejects(readState({ github, context }), /API unavailable/)
  } finally {
    fs.rmSync(directory, { recursive: true, force: true })
  }
})

test('records a notification only after Slack acknowledges it', async () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'monitor-test-'))
  const cwd = process.cwd()
  const originalFetch = global.fetch
  const originalEnv = { ...process.env }
  try {
    process.chdir(directory)
    process.env.CHECK_RESULTS = JSON.stringify(results('assets'))
    process.env.SLACK_PROD_ALERTS_WEBHOOK_URL =
      'https://example.invalid/webhook'
    const options = { github: mockGithub(), context, core: { info() {} } }
    global.fetch = async () => ({
      ok: false,
      status: 503,
      text: async () => 'error',
    })
    await assert.rejects(notify(options), /Slack notification failed/)
    assert.equal(fs.existsSync('release-monitor-state.json'), false)
    global.fetch = async () => {
      throw new Error('timeout')
    }
    await assert.rejects(notify(options), /timeout/)
    assert.equal(fs.existsSync('release-monitor-state.json'), false)
    global.fetch = async (_, options) => {
      const text = JSON.parse(options.body).text
      assert.match(text, /FAILURE/)
      assert.match(text, /Failed: check_latest_release/)
      assert.match(text, /Not run: test_install_paths \(skipped\)/)
      assert.doesNotMatch(text, /Failed: test_install_paths/)
      return { ok: true, status: 200, text: async () => 'ok' }
    }
    await notify(options)
    assert.deepEqual(
      JSON.parse(fs.readFileSync('release-monitor-state.json')),
      { status: 'failure' },
    )
    execFileSync('zip', ['-q', 'state.zip', 'release-monitor-state.json'])
    options.github = mockGithub(
      [{ id: 1, workflow_run: { id: 122, head_branch: 'main' } }],
      fs.readFileSync('state.zip'),
    )
    global.fetch = async () => {
      throw new Error('Unexpected duplicate alert')
    }
    await notify(options)
    process.env.CHECK_RESULTS = JSON.stringify(results())
    let recoveries = 0
    global.fetch = async (_, request) => {
      assert.match(JSON.parse(request.body).text, /RECOVERED/)
      recoveries++
      return { ok: true, status: 200, text: async () => 'ok' }
    }
    await notify(options)
    assert.equal(recoveries, 1)
    assert.deepEqual(
      JSON.parse(fs.readFileSync('release-monitor-state.json')),
      { status: 'healthy' },
    )
  } finally {
    process.chdir(cwd)
    global.fetch = originalFetch
    process.env = originalEnv
    fs.rmSync(directory, { recursive: true, force: true })
  }
})

test('installer retries script fetch and execution failures, but reports persistent failures', () => {
  const workflow = fs.readFileSync(
    path.join(__dirname, '../workflows/releases-test.yaml'),
    'utf8',
  )
  const step = workflow
    .split('      - name: Test curl|bash installer\n')[1]
    .split('      - name:')[0]
  const script = step
    .split('        run: |\n')[1]
    .split('\n')
    .filter((line) => line.startsWith('          '))
    .map((line) => line.slice(10))
    .join('\n')
  for (const [mode, failures, expectedAttempts, expectedOk] of [
    ['fetch', 2, 3, true],
    ['execution', 2, 3, true],
    ['fetch', 3, 3, false],
    ['execution', 3, 3, false],
    ['fetch', 0, 1, true],
    ['binary', 2, 3, true],
    ['binary', 3, 3, false],
    ['missing', 2, 3, true],
    ['missing', 3, 3, false],
  ]) {
    const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'monitor-test-'))
    try {
      for (const [name, target] of Object.entries({
        bash: '/bin/bash',
        cat: '/bin/cat',
        rm: '/bin/rm',
      })) {
        fs.symlinkSync(target, path.join(directory, name))
      }
      for (const [name, content] of Object.entries({
        curl: `#!/bin/bash
n=$(cat "$ATTEMPTS" 2>/dev/null || echo 0)
n=$((n+1))
echo "$n" > "$ATTEMPTS"
if [ "$n" -le "$FAILURES" ]; then
  case "$MODE" in
    fetch) exit 35 ;;
    execution) echo "exit 1"; exit 0 ;;
    missing) echo "exit 0"; exit 0 ;;
  esac
fi
cat <<'INSTALL'
printf '#!/bin/bash\\nif [ "$MODE" = binary ] && [ "$(cat "$ATTEMPTS")" -le "$FAILURES" ]; then exit 1; fi\\necho "mirrord 1.2.3"\\n' > "$MOCK_BIN/mirrord"
/bin/chmod +x "$MOCK_BIN/mirrord"
INSTALL
`,
        sleep: '#!/bin/sh\nexit 0\n',
      }))
        fs.writeFileSync(path.join(directory, name), content, { mode: 0o755 })
      const output = path.join(directory, 'output')
      const attempts = path.join(directory, 'attempts')
      const run = spawnSync(
        '/bin/bash',
        ['-e', '-o', 'pipefail', '-c', script],
        {
          env: {
            ...process.env,
            PATH: directory,
            MOCK_BIN: directory,
            MODE: mode,
            FAILURES: String(failures),
            ATTEMPTS: attempts,
            GITHUB_OUTPUT: output,
          },
          encoding: 'utf8',
        },
      )
      assert.equal(run.status, 0, run.stderr)
      assert.equal(Number(fs.readFileSync(attempts)), expectedAttempts)
      assert.match(
        fs.readFileSync(output, 'utf8'),
        new RegExp(`ok=${expectedOk}`),
      )
    } finally {
      fs.rmSync(directory, { recursive: true, force: true })
    }
  }
})

test('restores the newest state across unordered pages and ignores other workflows', async () => {
  const github = mockGithub()
  github.paginate.iterator = async function* () {
    yield {
      data: [
        {
          id: 9,
          created_at: '2026-09-21T00:00:00Z',
          workflow_run: { id: 123, head_branch: 'main' },
        },
        {
          id: 11,
          created_at: '2026-09-21T02:00:00Z',
          workflow_run: { id: 999, head_branch: 'main' },
        },
      ],
    }
    yield {
      data: [
        {
          id: 10,
          created_at: '2026-09-21T01:00:00Z',
          workflow_run: { id: 123, head_branch: 'main' },
        },
      ],
    }
  }
  github.rest.actions.getWorkflowRun = async ({ run_id }) => ({
    data: { workflow_id: run_id === 999 ? 99 : 42 },
  })
  github.rest.actions.downloadArtifact = async ({ artifact_id }) => {
    assert.equal(artifact_id, 10)
    throw new Error('Selected newest state')
  }
  await assert.rejects(readState({ github, context }), /Selected newest state/)
})

test('cleanup keeps the replacement and never deletes other workflow or branch state', async () => {
  const github = mockGithub([
    {
      id: 9,
      created_at: '2026-09-21T00:00:00Z',
      workflow_run: { id: 123, head_branch: 'main' },
    },
    {
      id: 10,
      created_at: '2026-09-21T01:00:00Z',
      workflow_run: { id: 123, head_branch: 'main' },
    },
    {
      id: 11,
      created_at: '2026-09-21T02:00:00Z',
      workflow_run: { id: 999, head_branch: 'main' },
    },
    {
      id: 12,
      created_at: '2026-09-21T03:00:00Z',
      workflow_run: { id: 123, head_branch: 'feature' },
    },
  ])
  github.rest.actions.getWorkflowRun = async ({ run_id }) => ({
    data: { workflow_id: run_id === 999 ? 99 : 42 },
  })
  const deleted = []
  github.rest.actions.deleteArtifact = async ({ artifact_id }) =>
    deleted.push(artifact_id)
  await assert.rejects(
    pruneState({ github, context, artifactId: 13 }),
    /refusing cleanup/,
  )
  assert.deepEqual(deleted, [])
  await pruneState({ github, context, artifactId: 10 })
  assert.deepEqual(deleted, [9])
})
