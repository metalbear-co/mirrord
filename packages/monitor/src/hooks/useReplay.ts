import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { api } from '../api'
import type {
  CapturedRequest,
  CapturedRequestSummary,
  ReplayRun,
} from '../types'

// The session has no event for a captured request, so the list is refetched.
const POLL_INTERVAL_MS = 2500
// The session keeps this many replays of each request.
const KEPT_REPLAYS = 5

export interface UseReplay {
  requests: CapturedRequestSummary[]
  loadError: boolean
  /** Whether the session was started by a mirrord version that doesn't keep requests. */
  unsupported: boolean
  selectedId: number | null
  /** The selected request, or null while it loads or when loading it failed. */
  selected: CapturedRequest | null
  selectedError: string | null
  select: (id: number | null) => void
  /** Whether a replay of the selected request waits for the application. */
  sending: boolean
  /** Why the last replay of the selected request failed. */
  sendError: string | null
  send: () => Promise<void>
}

function message(err: unknown): string {
  return err instanceof Error ? err.message : String(err)
}

/**
 * Whether `detail` holds exactly the replays that the session keeps of a request that was
 * replayed `replayCount` times: the latest ones, without a gap.
 */
function holdsKeptReplays(
  detail: CapturedRequest,
  replayCount: number,
): boolean {
  const kept = Math.min(replayCount, KEPT_REPLAYS)
  const first = replayCount - kept + 1

  return (
    detail.replays.length === kept &&
    detail.replays.every((run, index) => run.number === first + index)
  )
}

/** Whether the list shows something about a request that its loaded detail doesn't have yet. */
function isStale(
  detail: CapturedRequest,
  summary: CapturedRequestSummary,
): boolean {
  return (
    detail.status !== summary.status ||
    detail.duration_ms !== summary.duration_ms ||
    // A replay that was made elsewhere, or whose answer was dropped, is missing from the detail.
    !holdsKeptReplays(detail, summary.replay_count) ||
    detail.not_replayable !== summary.not_replayable ||
    detail.body_size !== summary.body_size
  )
}

/**
 * Adds the replay `run` to `request`. A refresh of the request can arrive before the answer to
 * the replay does, so the request can already hold the run, or a later one.
 */
function withRun(request: CapturedRequest, run: ReplayRun): CapturedRequest {
  const replays = [
    ...request.replays.filter((kept) => kept.number !== run.number),
    run,
  ]
    .sort((a, b) => a.number - b.number)
    .slice(-KEPT_REPLAYS)

  return {
    ...request,
    replay_count: Math.max(request.replay_count, run.number),
    replays,
  }
}

/** A replay of one request that waits for the application, or why the last one failed. */
type SendState = 'sending' | { error: string }

/** Lists the requests of a session and replays them. Fetches only while `active`. */
export function useReplay(sessionId: string, active: boolean): UseReplay {
  const [requests, setRequests] = useState<CapturedRequestSummary[]>([])
  const [loadError, setLoadError] = useState(false)
  const [unsupported, setUnsupported] = useState(false)

  const [selectedId, setSelectedId] = useState<number | null>(null)
  const [selected, setSelected] = useState<CapturedRequest | null>(null)
  const [selectedError, setSelectedError] = useState<string | null>(null)

  // Tracked per request, so that a replay that waits on a breakpoint doesn't disable the button
  // of another request or show its error there.
  const [sendStates, setSendStates] = useState<
    Record<number, SendState | undefined>
  >({})

  const selectedIdRef = useRef(selectedId)
  selectedIdRef.current = selectedId

  const selectedRef = useRef(selected)
  selectedRef.current = selected

  // Request ids start at 1 in every session, so an answer about another session must be dropped.
  const sessionIdRef = useRef(sessionId)
  sessionIdRef.current = sessionId

  // Counts the session switches. A replay that started before a switch must not change the send
  // states after it, also when the user is back on the session of that replay.
  const switchCountRef = useRef(0)

  useEffect(() => {
    setRequests([])
    setLoadError(false)
    setUnsupported(false)
    setSelectedId(null)
    setSelected(null)
    setSelectedError(null)
    setSendStates({})
    switchCountRef.current += 1
  }, [sessionId])

  const loadSelected = useCallback(
    async (id: number) => {
      const isCurrent = () =>
        sessionIdRef.current === sessionId && selectedIdRef.current === id

      try {
        const request = await api.getCapturedRequest(sessionId, id)
        if (!isCurrent()) return

        // An answer that was read before a replay finished must not hide that replay again.
        setSelected((current) =>
          current?.id === id && current.replay_count > request.replay_count
            ? current
            : request,
        )
        setSelectedError(null)
      } catch (err) {
        if (isCurrent()) setSelectedError(message(err))
      }
    },
    [sessionId],
  )

  useEffect(() => {
    if (!active) return undefined
    let cancelled = false

    const poll = async () => {
      if (document.hidden) return
      try {
        const listed = await api.listCapturedRequests(sessionId)
        if (cancelled) return

        const next = listed ?? []
        setRequests(next)
        setUnsupported(listed === null)
        setLoadError(false)

        // The open request can change after it was loaded, for example when the application
        // answers it.
        const detail = selectedRef.current
        const summary = next.find((request) => request.id === detail?.id)
        if (detail && summary && isStale(detail, summary)) {
          void loadSelected(detail.id)
        }
      } catch {
        if (!cancelled) setLoadError(true)
      }
    }

    void poll()
    const interval = setInterval(() => void poll(), POLL_INTERVAL_MS)
    return () => {
      cancelled = true
      clearInterval(interval)
    }
  }, [sessionId, active, loadSelected])

  const select = useCallback(
    (id: number | null) => {
      selectedIdRef.current = id
      setSelectedId(id)
      setSelected(null)
      setSelectedError(null)
      if (id !== null) void loadSelected(id)
    },
    [loadSelected],
  )

  const send = useCallback(async () => {
    const id = selectedIdRef.current
    if (id === null) return

    const setSendState = (state: SendState | undefined) =>
      setSendStates((states) => ({ ...states, [id]: state }))

    const switchCount = switchCountRef.current
    const switched = () => switchCountRef.current !== switchCount

    setSendState('sending')
    try {
      const run = await api.replayRequest(sessionId, id)
      if (switched()) return

      setSelected((request) =>
        request?.id === id ? withRun(request, run) : request,
      )
      setSendState(undefined)
    } catch (err) {
      if (switched()) return
      setSendState({ error: message(err) })
    }
  }, [sessionId])

  const sendState = selectedId === null ? undefined : sendStates[selectedId]
  const sending = sendState === 'sending'
  const sendError = typeof sendState === 'object' ? sendState.error : null

  // The same object while nothing changed, so that the pane can skip rendering.
  return useMemo(
    () => ({
      requests,
      loadError,
      unsupported,
      selectedId,
      selected,
      selectedError,
      select,
      sending,
      sendError,
      send,
    }),
    [
      requests,
      loadError,
      unsupported,
      selectedId,
      selected,
      selectedError,
      select,
      sending,
      sendError,
      send,
    ],
  )
}
