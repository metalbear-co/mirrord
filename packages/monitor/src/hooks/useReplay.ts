import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { api } from '../api'
import type {
  CapturedRequest,
  CapturedRequestSummary,
  ReplayRun,
} from '../types'

// The session has no event for a captured request, so the list is refetched.
const POLL_INTERVAL_MS = 2500
// The replays that the session keeps of each request, `MAX_KEPT_REPLAYS` in the intproxy's
// `replay.rs`. Only the merge of a replay's answer uses it, not the check for a stale request.
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
 * Whether `detail` holds the latest replays of a request that was replayed `replayCount` times,
 * without a gap.
 */
function holdsLatestReplays(
  detail: CapturedRequest,
  replayCount: number,
): boolean {
  const first = replayCount - detail.replays.length + 1

  return (
    (replayCount === 0 || detail.replays.length > 0) &&
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
    !holdsLatestReplays(detail, summary.replay_count) ||
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

function sameSummary(
  a: CapturedRequestSummary,
  b: CapturedRequestSummary,
): boolean {
  const keys = Object.keys(b) as (keyof CapturedRequestSummary)[]
  return (
    keys.length === Object.keys(a).length &&
    keys.every((key) => a[key] === b[key])
  )
}

/**
 * Returns `next` with each summary that didn't change replaced by the one in `current`, and
 * `current` itself when nothing changed, so that the rows that didn't change skip rendering.
 */
function reuseUnchanged(
  current: CapturedRequestSummary[],
  next: CapturedRequestSummary[],
): CapturedRequestSummary[] {
  const byId = new Map(current.map((summary) => [summary.id, summary]))
  let changed = current.length !== next.length
  const merged = next.map((summary, index) => {
    const previous = byId.get(summary.id)
    if (previous === undefined || !sameSummary(previous, summary)) {
      changed = true
      return summary
    }
    if (current[index] !== previous) changed = true
    return previous
  })

  return changed ? merged : current
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

  // The fetch of the open request that waits for its answer. An answer to an earlier fetch is
  // dropped, because it can arrive after a newer one.
  const detailFetchRef = useRef<object | null>(null)

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
      const fetch = {}
      detailFetchRef.current = fetch

      const isCurrent = () =>
        sessionIdRef.current === sessionId &&
        selectedIdRef.current === id &&
        detailFetchRef.current === fetch

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
      } finally {
        if (detailFetchRef.current === fetch) detailFetchRef.current = null
      }
    },
    [sessionId],
  )

  useEffect(() => {
    if (!active) return undefined
    let cancelled = false

    const fetchList = async () => {
      try {
        const listed = await api.listCapturedRequests(sessionId)
        if (cancelled) return

        const next = listed ?? []
        setRequests((current) => reuseUnchanged(current, next))
        setUnsupported(listed === null)
        setLoadError(false)

        // The open request is loaded again when it changed after it was loaded, for example
        // when the application answers it, and when loading it failed. A fetch of it that still
        // waits for its answer covers both.
        const openId = selectedIdRef.current
        const detail = selectedRef.current
        const summary = next.find((request) => request.id === openId)
        if (
          openId !== null &&
          summary &&
          (detail === null || isStale(detail, summary)) &&
          detailFetchRef.current === null
        ) {
          void loadSelected(openId)
        }
      } catch {
        if (!cancelled) setLoadError(true)
      }
    }

    // One fetch of the list at a time, so that a slow answer can't replace a newer one.
    let pending = false
    const poll = () => {
      if (document.hidden || pending) return

      pending = true
      void fetchList().finally(() => {
        pending = false
      })
    }

    poll()
    const interval = setInterval(poll, POLL_INTERVAL_MS)
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
      // The merge shows the run at once, but it can't add the replays made elsewhere meanwhile,
      // so the request is loaded again. If that fails, the merged request stays.
      if (selectedIdRef.current === id) void loadSelected(id)
    } catch (err) {
      if (switched()) return
      setSendState({ error: message(err) })
    }
  }, [sessionId, loadSelected])

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
