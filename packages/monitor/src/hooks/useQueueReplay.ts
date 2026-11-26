import { useCallback, useEffect, useRef, useState } from 'react'
import { api } from '../api'
import type { KeptQueueMessage } from '../types'

const POLL_INTERVAL_MS = 2500
const FAILED_POLL_INTERVAL_MS = 15000

export interface UseQueueReplay {
  messages: KeptQueueMessage[]
  /** Why loading the messages failed, for example that the operator doesn't keep them. */
  loadError: string | null
  /** The message being published again, if any. */
  sendingId: number | null
  sendError: { id: number; detail: string } | null
  send: (id: number) => Promise<void>
}

function message(err: unknown): string {
  return err instanceof Error ? err.message : String(err)
}

/**
 * Lists the queue messages that the operator kept for a session and publishes them again.
 * Fetches only while `active`.
 */
export function useQueueReplay(
  sessionId: string,
  active: boolean,
): UseQueueReplay {
  const [messages, setMessages] = useState<KeptQueueMessage[]>([])
  const [loadError, setLoadError] = useState<string | null>(null)
  const [sendingId, setSendingId] = useState<number | null>(null)
  const [sendError, setSendError] = useState<{
    id: number
    detail: string
  } | null>(null)

  const sessionIdRef = useRef(sessionId)
  sessionIdRef.current = sessionId

  // The last list shown, as JSON, so that a poll that changes nothing doesn't render again.
  const shownRef = useRef('')

  useEffect(() => {
    shownRef.current = ''
    setMessages([])
    setLoadError(null)
    setSendingId(null)
    setSendError(null)
  }, [sessionId])

  const load = useCallback(async (): Promise<boolean> => {
    const requested = sessionId
    try {
      const listed = await api.listQueueMessages(requested)
      if (sessionIdRef.current !== requested) return true
      const json = JSON.stringify(listed)
      if (json !== shownRef.current) {
        shownRef.current = json
        setMessages(listed)
      }
      setLoadError(null)
      return true
    } catch (err) {
      if (sessionIdRef.current !== requested) return true
      setLoadError(message(err))
      return false
    }
  }, [sessionId])

  // Each poll starts after the last one ended, so slow answers never pile up. After a failure,
  // such as queue replay being off on the operator, it waits longer.
  useEffect(() => {
    if (!active) return
    let stopped = false
    let timer: ReturnType<typeof setTimeout> | undefined

    const poll = async () => {
      const ok = document.hidden ? true : await load()
      if (stopped) return
      timer = setTimeout(
        () => void poll(),
        ok ? POLL_INTERVAL_MS : FAILED_POLL_INTERVAL_MS,
      )
    }
    void poll()

    return () => {
      stopped = true
      clearTimeout(timer)
    }
  }, [active, load])

  const send = useCallback(
    async (id: number) => {
      const requested = sessionId
      setSendingId(id)
      setSendError(null)
      try {
        await api.replayQueueMessage(requested, id)
        if (sessionIdRef.current !== requested) return
        await load()
      } catch (err) {
        if (sessionIdRef.current !== requested) return
        setSendError({ id, detail: message(err) })
      } finally {
        if (sessionIdRef.current === requested) setSendingId(null)
      }
    },
    [sessionId, load],
  )

  return { messages, loadError, sendingId, sendError, send }
}
