import { useEffect, useRef, useState } from 'react'
import { api } from '../api'
import type { QueueSplitView, SessionInfo } from '../types'

const POLL_INTERVAL_MS = 5000

export type QueueSplitsState =
  | { status: 'loading' }
  | { status: 'ready'; splits: QueueSplitView[] }
  | { status: 'unavailable'; reason: string }

export interface UseQueueSplits {
  state: QueueSplitsState
  splitCount: number
}

export function useQueueSplits(
  session: SessionInfo,
  enabled: boolean,
): UseQueueSplits {
  const [state, setState] = useState<QueueSplitsState>({ status: 'loading' })
  const sessionId = session.session_id
  const context = session.context ?? null
  const namespace = session.namespace ?? null
  const isOperator = session.is_operator
  const stateRef = useRef(state)
  stateRef.current = state

  useEffect(() => {
    setState({ status: 'loading' })
    if (!enabled || !isOperator) return

    let cancelled = false

    const poll = async () => {
      try {
        const resp = await api.listQueueSplits(context, namespace, sessionId)
        if (cancelled) return
        if (resp.status === 'available') {
          setState({ status: 'ready', splits: resp.splits })
        } else {
          setState({
            status: 'unavailable',
            reason: resp.reason ?? 'operator unavailable',
          })
        }
      } catch (err) {
        if (cancelled) return
        if (stateRef.current.status !== 'ready') {
          setState({
            status: 'unavailable',
            reason: err instanceof Error ? err.message : String(err),
          })
        }
      }
    }

    void poll()
    const interval = setInterval(() => void poll(), POLL_INTERVAL_MS)
    return () => {
      cancelled = true
      clearInterval(interval)
    }
  }, [sessionId, context, namespace, isOperator, enabled])

  const splitCount = state.status === 'ready' ? state.splits.length : 0
  return { state, splitCount }
}
