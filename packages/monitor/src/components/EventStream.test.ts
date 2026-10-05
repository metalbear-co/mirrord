import { describe, expect, it } from 'vitest'
import { EventType } from '../eventTypes'
import type { MonitorEvent } from '../types'
import { MAX_EVENTS } from './events/eventConfig'
import { appendToEventBuffer } from './EventStream'

function timestamped(event: MonitorEvent, seq: number) {
  return { event, receivedAt: new Date(seq), seq }
}

describe('appendToEventBuffer', () => {
  it('replaces a port count without removing visible history', () => {
    const events = Array.from({ length: MAX_EVENTS - 1 }, (_, seq) =>
      timestamped({ type: EventType.DnsQuery, host: `service-${seq}` }, seq),
    )
    events.push(
      timestamped(
        {
          type: EventType.PortSubscription,
          port: 80,
          mode: 'steal',
          hit_count: 1,
        },
        MAX_EVENTS - 1,
      ),
    )

    const buffered = appendToEventBuffer(
      events,
      timestamped(
        {
          type: EventType.PortSubscription,
          port: 80,
          mode: 'steal',
          hit_count: 2,
        },
        MAX_EVENTS,
      ),
    )

    expect(buffered).toHaveLength(MAX_EVENTS)
    expect(buffered[0]?.event).toEqual({
      type: EventType.DnsQuery,
      host: 'service-0',
    })
    expect(buffered.at(-1)?.event).toEqual({
      type: EventType.PortSubscription,
      port: 80,
      mode: 'steal',
      hit_count: 2,
    })
  })

  it('keeps the latest count for each port', () => {
    const buffered = appendToEventBuffer(
      [
        timestamped(
          {
            type: EventType.PortSubscription,
            port: 80,
            mode: 'steal',
            hit_count: 1,
          },
          0,
        ),
        timestamped(
          {
            type: EventType.PortSubscription,
            port: 81,
            mode: 'mirror',
            hit_count: 3,
          },
          1,
        ),
      ],
      timestamped(
        {
          type: EventType.PortSubscription,
          port: 80,
          mode: 'steal',
          hit_count: 2,
        },
        2,
      ),
    )

    expect(buffered.map(({ event }) => event)).toEqual([
      {
        type: EventType.PortSubscription,
        port: 81,
        mode: 'mirror',
        hit_count: 3,
      },
      {
        type: EventType.PortSubscription,
        port: 80,
        mode: 'steal',
        hit_count: 2,
      },
    ])
  })
})
