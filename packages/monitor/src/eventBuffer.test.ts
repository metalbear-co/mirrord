import { describe, expect, it } from 'vitest'
import { EventBuffer, MAX_ROWS } from './eventBuffer'
import type { SubscribeEventRow } from './subscribeEvents'

let seq = 0

function request(correlation: string, sessionKey = 'alice'): SubscribeEventRow {
  return {
    seq: seq++,
    timestamp: '2026-01-01T00:00:00Z',
    sessionKey,
    serviceName: 'checkout',
    type: 'http',
    source: 'GET /health',
    status: 'stolen',
    filtered: false,
    correlation,
    awaitingResponse: true,
  }
}

function response(
  correlation: string,
  status = '200',
  sessionKey = 'alice',
): SubscribeEventRow {
  return {
    ...request(correlation, sessionKey),
    type: 'http_response',
    source: '',
    status,
    awaitingResponse: false,
  }
}

function queueMessage(): SubscribeEventRow {
  return {
    ...request(''),
    type: 'sqs',
    source: 'orders',
    awaitingResponse: false,
  }
}

describe('EventBuffer', () => {
  it('completes a request with the response answering it', () => {
    const buffer = new EventBuffer()
    buffer.absorb([request('4:1')])
    const rows = buffer.absorb([response('4:1', '204')])

    expect(rows).toHaveLength(1)
    expect(rows[0]?.status).toBe('204')
    expect(rows[0]?.awaitingResponse).toBe(false)
    expect(rows[0]?.source).toBe('GET /health')
  })

  it('pairs across any number of intervening rows', () => {
    const buffer = new EventBuffer()
    buffer.absorb([request('4:1')])
    for (let i = 0; i < 2_000; i += 1) buffer.absorb([queueMessage()])
    const rows = buffer.absorb([response('4:1', '500')])

    expect(rows).toHaveLength(2_001)
    expect(rows[0]?.status).toBe('500')
    expect(rows.filter((row) => row.type === 'http_response')).toHaveLength(0)
  })

  it('keeps a response whose request was never seen', () => {
    const buffer = new EventBuffer()
    const rows = buffer.absorb([response('9:9')])

    expect(rows).toHaveLength(1)
    expect(rows[0]?.type).toBe('http_response')
  })

  it('does not pair across sessions that share a correlation', () => {
    const buffer = new EventBuffer()
    buffer.absorb([request('4:1', 'alice')])
    const rows = buffer.absorb([response('4:1', '200', 'bob')])

    expect(rows).toHaveLength(2)
    expect(rows[0]?.awaitingResponse).toBe(true)
  })

  it('pairs with the newest request when a correlation is reused', () => {
    const buffer = new EventBuffer()
    buffer.absorb([request('4:1')])
    const second = request('4:1')
    buffer.absorb([second])
    const rows = buffer.absorb([response('4:1', '404')])

    expect(rows[0]?.awaitingResponse).toBe(true)
    expect(rows[1]?.seq).toBe(second.seq)
    expect(rows[1]?.status).toBe('404')
  })

  it('drops the oldest rows past the cap', () => {
    const buffer = new EventBuffer()
    let rows: SubscribeEventRow[] = []
    for (let i = 0; i < MAX_ROWS + 10; i += 1)
      rows = buffer.absorb([request('')])

    expect(rows).toHaveLength(MAX_ROWS)
  })

  it('finds a request that survived an eviction', () => {
    const buffer = new EventBuffer()
    for (let i = 0; i < 10; i += 1) buffer.absorb([queueMessage()])
    buffer.absorb([request('4:1')])
    for (let i = 0; i < MAX_ROWS - 5; i += 1) buffer.absorb([queueMessage()])
    const rows = buffer.absorb([response('4:1', '201')])

    expect(rows).toHaveLength(MAX_ROWS)
    expect(rows[4]?.status).toBe('201')
    expect(rows.filter((row) => row.type === 'http_response')).toHaveLength(0)
  })

  it('keeps a response whose request has been dropped', () => {
    const buffer = new EventBuffer()
    buffer.absorb([request('4:1')])
    for (let i = 0; i < MAX_ROWS; i += 1) buffer.absorb([queueMessage()])
    const rows = buffer.absorb([response('4:1')])

    expect(rows).toHaveLength(MAX_ROWS)
    expect(rows.at(-1)?.type).toBe('http_response')
  })

  it('forgets everything on clear', () => {
    const buffer = new EventBuffer()
    buffer.absorb([request('4:1')])
    buffer.clear()
    const rows = buffer.absorb([response('4:1')])

    expect(rows).toHaveLength(1)
    expect(rows[0]?.type).toBe('http_response')
  })
})
