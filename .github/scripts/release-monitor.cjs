const fs = require('node:fs')
const os = require('node:os')
const path = require('node:path')
const { execFileSync } = require('node:child_process')

const artifactName = 'release-monitor-state'
const stateFile = `${artifactName}.json`

// An incident stays open across changes in release tag or failing checks. Optional
// asset warnings can escalate to failures; recovery requires every check to pass.
function transition(previous, results) {
  const checks = Object.values(results)
  const optional = results.check_latest_release.outputs.missing_optional
  let status = checks.some((check) => check.result !== 'success')
    ? 'failure'
    : optional
      ? 'warning'
      : 'healthy'
  if (previous?.status === 'failure' && status === 'warning') status = 'failure'
  const notify =
    status === 'healthy'
      ? previous != null && previous.status !== 'healthy'
      : previous == null ||
        previous.status === 'healthy' ||
        (previous.status === 'warning' && status === 'failure')
  return { status, notify }
}

// Artifact pagination order is not a freshness guarantee. Select by upload time
// across all pages, including when an older workflow run is rerun.
async function stateArtifacts({ github, context }) {
  const { owner, repo } = context.repo
  const { data: currentRun } = await github.rest.actions.getWorkflowRun({
    owner,
    repo,
    run_id: context.runId,
  })
  const artifacts = []
  const workflows = new Map([[context.runId, currentRun.workflow_id]])
  for await (const page of github.paginate.iterator(
    github.rest.actions.listArtifactsForRepo,
    { owner, repo, name: artifactName, per_page: 100 },
  )) {
    for (const artifact of page.data) {
      if (
        artifact.expired ||
        artifact.workflow_run?.head_branch !==
          context.payload.repository.default_branch
      )
        continue
      const runId = artifact.workflow_run.id
      if (!workflows.has(runId)) {
        const { data: run } = await github.rest.actions.getWorkflowRun({
          owner,
          repo,
          run_id: runId,
        })
        workflows.set(runId, run.workflow_id)
      }
      if (workflows.get(runId) === currentRun.workflow_id)
        artifacts.push(artifact)
    }
  }
  return artifacts.sort(
    (a, b) =>
      Date.parse(b.created_at) - Date.parse(a.created_at) || b.id - a.id,
  )
}

async function readState({ github, context }) {
  const [artifact] = await stateArtifacts({ github, context })
  if (!artifact) return null
  const { data } = await github.rest.actions.downloadArtifact({
    ...context.repo,
    artifact_id: artifact.id,
    archive_format: 'zip',
  })
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'release-monitor-'))
  try {
    const archive = path.join(directory, 'state.zip')
    fs.writeFileSync(archive, Buffer.from(data))
    const state = JSON.parse(
      execFileSync('unzip', ['-p', archive, stateFile], { encoding: 'utf8' }),
    )
    if (!['healthy', 'warning', 'failure'].includes(state.status)) {
      throw new Error('Invalid release monitor state')
    }
    return state
  } finally {
    fs.rmSync(directory, { recursive: true, force: true })
  }
}

// Called only after upload succeeds, so cleanup cannot remove the last saved
// state when notification delivery or its replacement upload fails.
async function pruneState({ github, context, artifactId }) {
  const artifacts = await stateArtifacts({ github, context })
  if (artifacts[0]?.id !== artifactId) {
    throw new Error(
      'Uploaded state is not the latest artifact; refusing cleanup',
    )
  }
  for (const artifact of artifacts.slice(1)) {
    await github.rest.actions.deleteArtifact({
      ...context.repo,
      artifact_id: artifact.id,
    })
  }
}

async function notify({ github, context, core }) {
  const previous = await readState({ github, context })
  const results = JSON.parse(process.env.CHECK_RESULTS)
  const next = transition(previous, results)
  const {
    tag,
    html_url: releaseUrl,
    error,
    missing_optional: optional,
  } = results.check_latest_release.outputs
  const runUrl = `${context.serverUrl}/${context.repo.owner}/${context.repo.repo}/actions/runs/${context.runId}`
  if (next.notify) {
    const webhook = process.env.SLACK_PROD_ALERTS_WEBHOOK_URL
    if (!webhook) throw new Error('SLACK_PROD_ALERTS_WEBHOOK_URL is not set')
    const title = {
      healthy: ':white_check_mark: Release monitor RECOVERED',
      warning: ':warning: Release monitor WARNING',
      failure: ':rotating_light: Release monitor FAILURE',
    }[next.status]
    const details = Object.entries(results)
      .filter(([, check]) => check.result !== 'success')
      .map(([name, check]) => {
        if (check.result === 'skipped') return `• Not run: ${name} (skipped)`
        if (check.result === 'cancelled')
          return `• Not completed: ${name} (cancelled)`
        return `• Failed: ${name}`
      })
    const text = [
      `*${title} for \`${context.repo.owner}/${context.repo.repo}\`*`,
      `• Release: ${tag || 'unknown'}`,
      ...(next.status === 'healthy'
        ? [
            'All release assets, Linux/macOS installers, and version endpoint checks are passing.',
          ]
        : [
            ...details,
            ...(error ? [error] : []),
            ...(optional
              ? [`Missing optional Windows assets: ${optional}`]
              : []),
          ]),
      ...(releaseUrl ? [`<${releaseUrl}|Release page>`] : []),
      `<${runUrl}|Monitor run and logs>`,
    ].join('\n')
    const response = await fetch(webhook, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ text }),
      signal: AbortSignal.timeout(15000),
    })
    const body = await response.text()
    if (!response.ok || body.trim() !== 'ok') {
      throw new Error(`Slack notification failed (HTTP ${response.status})`)
    }
  } else {
    core.info(`Release monitor remains ${next.status}; no notification needed.`)
  }
  // Persist only after Slack acknowledges a transition. Failed delivery must be
  // retried on the next run instead of silently suppressing the incident.
  fs.writeFileSync(stateFile, JSON.stringify({ status: next.status }))
}

module.exports = { transition, readState, pruneState, notify }
