import { useEffect, useRef, useState } from 'react'
import { api } from '../api'
import type { MonitorEvent, SessionInfo } from '../types'

const MAX_HOSTS_PER_SESSION = 200

export function useSessionTraffic(
  sessions: SessionInfo[],
  enabled: boolean,
): Record<string, string[]> {
  const [traffic, setTraffic] = useState<Record<string, string[]>>({})
  const streamsRef = useRef(new Map<string, EventSource>())
  const hostsRef = useRef(new Map<string, Set<string>>())

  const sessionIds = sessions.map((s) => s.session_id).join(',')

  useEffect(() => {
    const streams = streamsRef.current
    const hosts = hostsRef.current
    const liveIds = new Set(sessionIds === '' ? [] : sessionIds.split(','))

    for (const [id, stream] of streams) {
      if (!enabled || !liveIds.has(id)) {
        stream.close()
        streams.delete(id)
      }
    }

    for (const id of hosts.keys()) {
      if (!liveIds.has(id)) hosts.delete(id)
    }
    setTraffic((prev) => {
      const live = Object.entries(prev).filter(([id]) => liveIds.has(id))
      return live.length === Object.keys(prev).length
        ? prev
        : Object.fromEntries(live)
    })

    if (!enabled) return

    for (const id of liveIds) {
      if (streams.has(id)) continue
      const stream = new EventSource(api.eventStreamUrl(id))
      streams.set(id, stream)
      stream.onmessage = (e) => {
        let event: MonitorEvent
        try {
          event = JSON.parse(e.data as string) as MonitorEvent
        } catch {
          return
        }
        if (event.type !== 'dns_query') return
        const seen = hosts.get(id) ?? new Set<string>()
        hosts.set(id, seen)
        if (seen.has(event.host) || seen.size >= MAX_HOSTS_PER_SESSION) return
        seen.add(event.host)
        setTraffic((prev) => ({ ...prev, [id]: [...seen] }))
      }
      stream.onerror = () => {
        stream.close()
        streams.delete(id)
      }
    }
  }, [sessionIds, enabled])

  useEffect(() => {
    const streams = streamsRef.current
    return () => {
      for (const stream of streams.values()) stream.close()
      streams.clear()
    }
  }, [])

  return traffic
}
