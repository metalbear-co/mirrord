import { describe, expect, it } from 'vitest'
import { isStale, reuseUnchanged, withRun } from './useReplay'
import type {
  CapturedRequest,
  CapturedRequestSummary,
  ReplayRun,
} from '../types'

function summary(
  id: number,
  replayCount = 0,
  status: number | null = 200,
): CapturedRequestSummary {
  return {
    id,
    captured_at_ms: 0,
    mode: 'steal',
    port: 80,
    method: 'GET',
    path: '/items',
    status,
    duration_ms: 1,
    body_size: 0,
    not_replayable: null,
    replay_count: replayCount,
  }
}

function run(number: number): ReplayRun {
  return {
    number,
    at_ms: number,
    response: { status: 200, headers: [], duration_ms: 1 },
    body: { text: '', size: 0, truncated: false },
  }
}

function detail(replays: number[], replayCount = replays.at(-1) ?? 0) {
  const request: CapturedRequest = {
    ...summary(1, replayCount),
    version: 'HTTP/1.1',
    headers: [],
    body: { text: '', size: 0, truncated: false },
    response: null,
    replays: replays.map(run),
  }
  return request
}

describe('isStale', () => {
  it('accepts the latest replays without a gap, however many are kept', () => {
    expect(isStale(detail([]), summary(1, 0))).toBe(false)
    expect(isStale(detail([3, 4, 5, 6, 7]), summary(1, 7))).toBe(false)
    expect(
      isStale(detail([3, 4, 5, 6, 7, 8, 9, 10, 11, 12]), summary(1, 12)),
    ).toBe(false)
  })

  it('catches a replay that the detail is missing', () => {
    expect(isStale(detail([1, 2, 3]), summary(1, 4))).toBe(true)
    expect(isStale(detail([1, 2, 4], 4), summary(1, 4))).toBe(true)
    expect(isStale(detail([], 2), summary(1, 2))).toBe(true)
  })

  it('catches a changed response', () => {
    expect(isStale(detail([]), summary(1, 0, null))).toBe(true)
  })
})

describe('withRun', () => {
  it('adds the run in order, once', () => {
    const merged = withRun(withRun(detail([1, 2]), run(3)), run(3))
    expect(merged.replays.map((kept) => kept.number)).toEqual([1, 2, 3])
    expect(merged.replay_count).toBe(3)
  })

  it('keeps a later count that a refresh already brought', () => {
    expect(withRun(detail([1, 2, 3, 4]), run(3)).replay_count).toBe(4)
  })
})

describe('reuseUnchanged', () => {
  it('keeps the list when nothing changed', () => {
    const current = [summary(1), summary(2)]
    expect(reuseUnchanged(current, [summary(1), summary(2)])).toBe(current)
  })

  it('keeps the summaries that did not change', () => {
    const current = [summary(1), summary(2)]
    const next = reuseUnchanged(current, [
      summary(1),
      summary(2, 1),
      summary(3),
    ])

    expect(next).not.toBe(current)
    expect(next[0]).toBe(current[0])
    expect(next[1]).toEqual(summary(2, 1))
    expect(next[2]).toEqual(summary(3))
  })
})
