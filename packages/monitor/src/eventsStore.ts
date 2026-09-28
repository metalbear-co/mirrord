import { useSyncExternalStore } from 'react'
import { api } from './api'
import type { ContextSelection } from './contextStore'
import { EventBuffer, MAX_ROWS } from './eventBuffer'
import { toEventRow, type SubscribeEventRow } from './subscribeEvents'

// The operator's interception events, collected app-wide from the moment the UI opens so the
// events view has history the first time it is shown. Module scope: the view is a lazily loaded
// chunk that must not own the stream.

/** The first operator release whose event stream names and pairs events. */
export const MINIMUM_OPERATOR_VERSION = '3.210.0'

/** Why the events view cannot show anything, when it cannot. */
export type Unavailable =
  | { kind: 'unreachable'; reason: string }
  | { kind: 'unsupported'; version: string }

/** Which messages the viewer wants to see, by what the operator did with them. */
export type Routing = 'consumed' | 'filtered' | 'both'

export interface EventsState {
  rows: SubscribeEventRow[]
  unavailable: Unavailable | null
  streaming: boolean
  routing: Routing
}

let state: EventsState = {
  rows: [],
  unavailable: null,
  streaming: false,
  routing: 'consumed',
}

const listeners = new Set<() => void>()
const buffer = new EventBuffer()

let stream: EventSource | null = null
let context: string | null = null
let seq = 0

// Rows are batched to one commit per frame. Frames stop in a hidden tab, so the batch is bounded
// too.
let pending: SubscribeEventRow[] = []
let flushHandle: number | null = null

function publish(next: Partial<EventsState>): void {
  state = { ...state, ...next }
  for (const listener of listeners) listener()
}

function flush(): void {
  flushHandle = null
  if (pending.length === 0) return

  const rows = buffer.absorb(pending)
  pending = []
  publish({ rows })
}

function enqueue(row: SubscribeEventRow): void {
  pending.push(row)
  if (pending.length >= MAX_ROWS) flush()
  else flushHandle ??= requestAnimationFrame(flush)
}

function open(): void {
  stream?.close()

  stream = new EventSource(
    api.operatorEventStreamUrl(context, state.routing !== 'consumed'),
  )

  stream.onopen = () => {
    publish({ streaming: true })
  }

  stream.onmessage = (message) => {
    let payload: unknown
    try {
      payload = JSON.parse(message.data as string)
    } catch {
      return
    }

    const row = toEventRow(payload, seq++)
    if (row) enqueue(row)
  }

  stream.addEventListener('status', (message) => {
    let status: unknown
    try {
      status = JSON.parse(message.data as string)
    } catch {
      return
    }

    const { available, supported, version, reason } = status as {
      available?: boolean
      supported?: boolean
      version?: string
      reason?: string
    }

    if (available === false) {
      publish({ unavailable: { kind: 'unreachable', reason: reason ?? '' } })
    } else if (supported === false) {
      publish({
        unavailable: { kind: 'unsupported', version: version ?? '' },
      })
    } else {
      publish({ unavailable: null })
    }
  })

  // EventSource reconnects on its own; closing it here would strand the view.
  stream.onerror = () => {
    publish({ streaming: false })
  }
}

/**
 * Collects for the selected context once the kubeconfig's contexts are known, re-targeting a
 * running collection when the selection changes.
 */
export function collectEvents({ current, selected }: ContextSelection): void {
  if (current === null) return

  const kubeContext = selected ?? current
  if (context === kubeContext) return

  if (context !== null) {
    pending = []
    buffer.clear()
    publish({ rows: [], unavailable: null })
  }

  context = kubeContext
  open()
}

/**
 * Chooses which messages to show. Only a stream asked for them reports messages no filter took, so
 * the stream is reopened when that changes.
 */
export function setRouting(routing: Routing): void {
  if (state.routing === routing) return

  const reopen = routing === 'consumed' || state.routing === 'consumed'
  publish({ routing })
  if (context !== null && reopen) open()
}

export function clearEvents(): void {
  pending = []
  buffer.clear()
  publish({ rows: [] })
}

export function useEvents(): EventsState {
  return useSyncExternalStore(
    (listener) => {
      listeners.add(listener)
      return () => listeners.delete(listener)
    },
    () => state,
  )
}
