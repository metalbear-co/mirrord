import { describe, expect, it } from 'vitest'
import { labelForType, toEventRow } from './subscribeEvents'

function event(data: unknown): unknown {
  return {
    session_key: '73b63',
    service_name: 'metal-mart-frontend',
    timestamp: '2026-08-23T12:57:01.740537Z',
    data,
  }
}

function queue(queueType: string, mode = 'steal'): unknown {
  return event({
    queue_message: {
      mode,
      queue_type: queueType,
      queue_name: 'orders',
      properties: {},
    },
  })
}

// Every `queue_type` the operator can put on a `queue_message`, with the label the table shows.
const QUEUE_TYPES: [string, string][] = [
  ['sqs', 'SQS'],
  ['azure_service_bus', 'Azure Service Bus'],
  ['bullmq', 'BullMQ'],
  ['rmq', 'RabbitMQ'],
  ['gcppubsub', 'GCP Pub/Sub'],
  ['nats', 'NATS'],
  ['natspubsub', 'NATS Pub/Sub'],
]

describe('toEventRow', () => {
  it('carries the envelope onto the row', () => {
    const row = toEventRow(event({ lagged: { count: 12 } }), 7)

    expect(row).toMatchObject({
      seq: 7,
      sessionKey: '73b63',
      serviceName: 'metal-mart-frontend',
      timestamp: '2026-08-23T12:57:01.740537Z',
    })
  })

  it('opens one row per call, awaiting the response that completes it', () => {
    const row = toEventRow(
      event({
        http_request: {
          mode: 'steal',
          connection_id: 4,
          request_id: 1,
          method: 'GET',
          uri: '/shop/',
        },
      }),
      0,
    )

    expect(row).toMatchObject({
      type: 'http',
      source: 'GET /shop/',
      status: 'stolen',
      correlation: '4:1',
      awaitingResponse: true,
    })
  })

  it('correlates a response to the request it answers', () => {
    const row = toEventRow(
      event({
        http_response: { connection_id: 4, request_id: 1, status: 500 },
      }),
      0,
    )

    expect(row).toMatchObject({
      type: 'http_response',
      status: '500',
      correlation: '4:1',
      awaitingResponse: false,
    })
  })

  it.each(QUEUE_TYPES)('keys a %s queue message by its broker', (queueType) => {
    expect(toEventRow(queue(queueType), 0)).toMatchObject({
      type: queueType,
      source: 'orders',
      status: 'stolen',
    })
  })

  it.each([
    ['steal', 'stolen'],
    ['mirror', 'mirrored'],
    ['filtered', 'filtered'],
  ])('reports %s as %s, in the past tense', (mode, label) => {
    expect(toEventRow(queue('sqs', mode), 0)).toMatchObject({
      status: label,
      filtered: mode === 'filtered',
    })
  })

  it('sources a kafka message from its topic', () => {
    const row = toEventRow(
      event({
        kafka_message: {
          mode: 'mirror',
          topic: 'payments.notifications',
          partition: 0,
          offset: 42,
          headers: {},
        },
      }),
      0,
    )

    expect(row).toMatchObject({
      type: 'kafka_message',
      source: 'payments.notifications',
      status: 'mirrored',
    })
  })

  it('sources a redis message from its channel', () => {
    const row = toEventRow(
      event({
        redis_message: {
          mode: 'steal',
          channel: 'orders.created',
          payload_size: 52,
        },
      }),
      0,
    )

    expect(row).toMatchObject({
      type: 'redis_message',
      source: 'orders.created',
      status: 'stolen',
    })
  })

  it('reports how many events a lagged subscriber dropped, with no status', () => {
    expect(toEventRow(event({ lagged: { count: 12 } }), 0)).toMatchObject({
      type: 'lagged',
      source: '12 events dropped',
      status: '',
    })
  })

  it('keeps an event type it does not recognise, under its tag', () => {
    expect(
      toEventRow(event({ temporal_message: { mode: 'steal' } }), 0),
    ).toMatchObject({ type: 'temporal_message', source: '', status: 'stolen' })
  })

  it('keeps a queue broker it does not recognise, under its name', () => {
    expect(toEventRow(queue('pulsar'), 0)).toMatchObject({
      type: 'pulsar',
      source: 'orders',
    })
  })

  it.each([
    ['a queue message with no broker', event({ queue_message: {} })],
    ['a keep-alive comment', 'ping'],
    ['an array', []],
    ['an untagged event', { session_key: 'k', data: {} }],
    ['an event without data', { session_key: 'k' }],
  ])('drops %s', (_name, raw) => {
    expect(toEventRow(raw, 0)).toBeNull()
  })
})

describe('labelForType', () => {
  it.each([
    ...QUEUE_TYPES,
    ['http', 'HTTP'],
    ['http_response', 'HTTP response'],
    ['kafka_message', 'Kafka'],
    ['redis_message', 'Redis Pub/Sub'],
    ['lagged', 'Lagged'],
  ])('labels %s as %s', (type, label) => {
    expect(labelForType(type)).toBe(label)
  })

  it('has no label for a type it does not recognise', () => {
    expect(labelForType('some_new_broker')).toBeNull()
  })
})

describe('a request and its response', () => {
  const call = (connection: number, request: number) => ({
    request: event({
      http_request: {
        mode: 'steal',
        connection_id: connection,
        request_id: request,
        method: 'GET',
        uri: '/shop/',
      },
    }),
    response: event({
      http_response: {
        connection_id: connection,
        request_id: request,
        status: 200,
      },
    }),
  })

  it('share a correlation key', () => {
    const { request, response } = call(7, 3)

    expect(toEventRow(request, 0)?.correlation).toBe('7:3')
    expect(toEventRow(response, 1)?.correlation).toBe('7:3')
  })

  it('do not collide across connections reusing a request id', () => {
    expect(toEventRow(call(7, 0).request, 0)?.correlation).not.toBe(
      toEventRow(call(8, 0).request, 1)?.correlation,
    )
  })

  it('leaves a response without ids uncorrelated, so it keeps its own row', () => {
    const row = toEventRow(event({ http_response: { status: 502 } }), 0)

    expect(row).toMatchObject({ correlation: '', status: '502' })
  })
})
