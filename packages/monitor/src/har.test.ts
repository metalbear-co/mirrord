import { describe, expect, it } from 'vitest'
import type { MonitorEvent, SessionInfo } from './types'
import { buildHar } from './har'

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

const exchange = (overrides?: {
  requestExtra?: Partial<Extract<MonitorEvent, { type: 'incoming_request' }>>
  withBodies?: boolean
}) => {
  const request: MonitorEvent = {
    type: 'incoming_request',
    id: '0:s:0:0',
    method: 'POST',
    path: '/api/login?next=%2Fhome&x=1',
    host: 'shop.test',
    port: 443,
    http_version: 'HTTP/2.0',
    headers: [
      ['cookie', 'sid=abc; theme=dark'],
      ['content-type', 'application/json'],
    ],
    ...overrides?.requestExtra,
  }
  const response: MonitorEvent = {
    type: 'incoming_response',
    id: '0:s:0:0',
    status: 200,
    method: 'POST',
    path: '/api/login?next=%2Fhome&x=1',
    http_version: 'HTTP/2.0',
    headers: [
      ['set-cookie', 'sid=next; Path=/; HttpOnly'],
      ['content-type', 'application/json'],
    ],
  }
  const events: { event: MonitorEvent; receivedAt: Date }[] = [
    { event: request, receivedAt: at('2026-07-07T10:01:00.000Z') },
    { event: response, receivedAt: at('2026-07-07T10:01:00.250Z') },
  ]
  if (overrides?.withBodies) {
    events.push(
      {
        event: {
          type: 'incoming_request_body',
          id: '0:s:0:0',
          method: 'POST',
          path: '/api/login?next=%2Fhome&x=1',
          body: '{"user":"han"}',
          truncated: false,
          bytes: 14,
        },
        receivedAt: at('2026-07-07T10:01:00.100Z'),
      },
      {
        event: {
          type: 'incoming_response_body',
          id: '0:s:0:0',
          status: 200,
          method: 'POST',
          path: '/api/login?next=%2Fhome&x=1',
          body: '{"ok":true}',
          truncated: true,
          bytes: 40000,
        },
        receivedAt: at('2026-07-07T10:01:00.250Z'),
      },
    )
  }
  return events
}

describe('buildHar', () => {
  it('derives the scheme from the target port and uses the captured HTTP version', () => {
    const har = buildHar(session, exchange())
    const entry = har.log.entries[0]!
    expect(entry.request.url).toBe('https://shop.test/api/login?next=%2Fhome&x=1')
    expect(entry.request.httpVersion).toBe('HTTP/2.0')
    expect(entry.response.httpVersion).toBe('HTTP/2.0')
  })

  it('parses cookies and the query string from headers and path', () => {
    const har = buildHar(session, exchange())
    const entry = har.log.entries[0]!
    expect(entry.request.cookies).toEqual([
      { name: 'sid', value: 'abc' },
      { name: 'theme', value: 'dark' },
    ])
    expect(entry.response.cookies).toEqual([{ name: 'sid', value: 'next' }])
    expect(entry.request.queryString).toEqual([
      { name: 'next', value: '/home' },
      { name: 'x', value: '1' },
    ])
  })

  it('reports unknown body sizes as -1 instead of claiming an empty body', () => {
    const har = buildHar(session, exchange())
    const entry = har.log.entries[0]!
    expect(entry.request.bodySize).toBe(-1)
    expect(entry.response.bodySize).toBe(-1)
    expect(entry.response.content.size).toBe(-1)
  })

  it('fills sizes and bodies from body events and marks truncated captures', () => {
    const har = buildHar(session, exchange({ withBodies: true }))
    const entry = har.log.entries[0]!
    expect(entry.request.bodySize).toBe(14)
    expect(entry.request.postData?.text).toBe('{"user":"han"}')
    expect(entry.response.bodySize).toBe(40000)
    expect(entry.response.content.size).toBe(40000)
    expect(entry.response.content.text).toBe('{"ok":true}')
    expect(entry.response.content.comment).toContain('truncated')
  })

  it('falls back to http and HTTP/1.1 when port and version are missing (stale CLI)', () => {
    const har = buildHar(
      session,
      exchange({ requestExtra: { port: undefined, http_version: undefined } }),
    )
    const entry = har.log.entries[0]!
    expect(entry.request.url).toBe('http://shop.test/api/login?next=%2Fhome&x=1')
    expect(entry.request.httpVersion).toBe('HTTP/1.1')
  })
})
