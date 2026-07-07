import { describe, expect, it } from 'vitest'
import { strFromU8, unzipSync } from 'fflate'
import type { Har } from 'har-format'
import type { MonitorEvent, SessionInfo } from './types'
import { REDACTED_VALUE, buildExportZip, buildSessionLog, redactEvent } from './export'

const session: SessionInfo = {
  session_id: 'abc123',
  target: 'deployment/shop',
  started_at: '2026-07-07T10:00:00Z',
  mirrord_version: '3.230.0',
  is_operator: false,
  processes: [],
  port_subscriptions: [],
  config: {},
}

const at = (iso: string) => new Date(iso)

const request: MonitorEvent = {
  type: 'incoming_request',
  id: '0:s:0:0',
  method: 'POST',
  path: '/api/login?next=%2Fhome',
  host: 'shop.test',
  port: 443,
  http_version: 'HTTP/2.0',
  headers: [
    ['Authorization', 'Bearer secret-token'],
    ['Cookie', 'sid=secret-cookie'],
    ['content-type', 'application/json'],
  ],
}

const response: MonitorEvent = {
  type: 'incoming_response',
  id: '0:s:0:0',
  status: 200,
  method: 'POST',
  path: '/api/login?next=%2Fhome',
  http_version: 'HTTP/2.0',
  headers: [
    ['set-cookie', 'sid=new-secret'],
    ['content-type', 'application/json'],
  ],
}

describe('redactEvent', () => {
  it('replaces sensitive header values case-insensitively and keeps the rest', () => {
    const redacted = redactEvent(request)
    if (!('headers' in redacted) || !redacted.headers) throw new Error('headers missing')
    expect(redacted.headers).toEqual([
      ['Authorization', REDACTED_VALUE],
      ['Cookie', REDACTED_VALUE],
      ['content-type', 'application/json'],
    ])
  })

  it('leaves events without headers untouched', () => {
    const event: MonitorEvent = { type: 'dns_query', host: 'shop.test' }
    expect(redactEvent(event)).toBe(event)
  })
})

describe('buildSessionLog', () => {
  it('records the dropped-event count and stamps events with received_at', () => {
    const log = buildSessionLog(session, [{ event: request, receivedAt: at('2026-07-07T10:01:00Z') }], {
      droppedEvents: 42,
      exportedAt: at('2026-07-07T11:00:00Z'),
    })
    expect(log.dropped_events).toBe(42)
    expect(log.exported_at).toBe('2026-07-07T11:00:00.000Z')
    expect(log.events).toHaveLength(1)
    expect(log.events[0]).toMatchObject({ received_at: '2026-07-07T10:01:00.000Z', type: 'incoming_request' })
  })
})

describe('buildExportZip', () => {
  it('bundles a redacted session log and HAR into one zip', () => {
    const events = [
      { event: request, receivedAt: at('2026-07-07T10:01:00Z') },
      { event: response, receivedAt: at('2026-07-07T10:01:01Z') },
    ]
    const { filename, data } = buildExportZip(session, events, {
      droppedEvents: 0,
      exportedAt: at('2026-07-07T11:00:00Z'),
    })
    expect(filename).toBe('mirrord-session-abc123-2026-07-07T11-00-00-000Z.zip')

    const entries = unzipSync(data)
    const names = Object.keys(entries).sort()
    expect(names).toEqual([
      'mirrord-session-abc123-2026-07-07T11-00-00-000Z.har',
      'mirrord-session-abc123-2026-07-07T11-00-00-000Z.json',
    ])

    const log = JSON.parse(strFromU8(entries[names[1]]!))
    const logHeaders = log.events[0].headers as [string, string][]
    expect(logHeaders.find(([name]) => name === 'Authorization')?.[1]).toBe(REDACTED_VALUE)

    const har = JSON.parse(strFromU8(entries[names[0]]!)) as Har
    const entry = har.log.entries[0]!
    expect(entry.request.headers.find((h) => h.name === 'Authorization')?.value).toBe(REDACTED_VALUE)
    expect(entry.response.headers.find((h) => h.name === 'set-cookie')?.value).toBe(REDACTED_VALUE)
  })
})
