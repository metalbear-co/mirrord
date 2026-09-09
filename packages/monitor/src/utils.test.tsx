import { describe, expect, it } from 'vitest'
import { renderToStaticMarkup } from 'react-dom/server'
import JsonHighlight from './components/JsonHighlight'
import { formatJson, normalizeSessions } from './utils'

const circular: Record<string, unknown> = {}
circular['self'] = circular

const UNSERIALIZABLE: [string, unknown, string][] = [
  ['undefined', undefined, 'null'],
  ['null', null, 'null'],
  ['function', () => 1, 'null'],
  ['symbol', Symbol('s'), 'null'],
  ['toJSON returning undefined', { toJSON: () => undefined }, 'null'],
  ['bigint', 10n, '10'],
  ['circular', circular, '[object Object]'],
  ['false', false, 'false'],
  ['string', 'text', '"text"'],
  ['nested', { a: [1, null] }, '{\n  "a": [\n    1,\n    null\n  ]\n}'],
]

describe('formatJson', () => {
  it.each(UNSERIALIZABLE)('renders %s', (_name, value, expected) => {
    expect(formatJson(value)).toBe(expected)
  })
})

describe('JsonHighlight', () => {
  it.each(UNSERIALIZABLE)('does not throw for %s', (_name, value) => {
    expect(() =>
      renderToStaticMarkup(<JsonHighlight value={value} />),
    ).not.toThrow()
  })
})

describe('normalizeSessions', () => {
  const session = (
    session_id: unknown,
    extra: Record<string, unknown> = {},
  ) => ({
    session_id,
    target: 'deployment/app',
    started_at: '2026-09-09T14:00:00Z',
    mirrord_version: '3.255.0',
    is_operator: false,
    processes: [],
    port_subscriptions: [],
    ...extra,
  })

  it('drops entries that cannot be keyed and coerces missing lists', () => {
    const normalized = normalizeSessions([
      session('ok'),
      session('no-lists', { processes: undefined, port_subscriptions: null }),
      { info: session('nested-wrapper') },
      session(123),
      null,
      'garbage',
    ])

    expect(normalized.map((s) => s.session_id)).toEqual(['ok', 'no-lists'])
    expect(normalized[1]?.processes).toEqual([])
    expect(normalized[1]?.port_subscriptions).toEqual([])
  })

  it('returns an empty list for a non-array body', () => {
    expect(normalizeSessions({ sessions: [] })).toEqual([])
  })
})
