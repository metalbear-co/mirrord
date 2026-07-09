import { useState, useEffect } from 'react'
import { SlidersHorizontal } from 'lucide-react'
import type { SessionInfo, MonitorEvent, PortSubscription, ProcessInfo } from '../types'
import { api } from '../api'
import { emitUserBlocked } from '../analytics'
import { EventType } from '../eventTypes'
import { expectArray } from '../utils'
import EventStream from './EventStream'
import SessionHeader from './SessionHeader'
import ConfigModal from './ConfigModal'
import JoinChip from './JoinBar'
import type { ExtensionState } from '../extensionBridge'

interface Props {
  session: SessionInfo
  onKill: () => void
  extensionState: ExtensionState
  onJoin: () => Promise<{ ok: boolean; error?: string }>
  onLeave: () => Promise<{ ok: boolean; error?: string }>
}

export default function SessionDetail({
  session,
  onKill,
  extensionState,
  onJoin,
  onLeave,
}: Props) {
  const [portSubs, setPortSubs] = useState<PortSubscription[]>([])
  const [processes, setProcesses] = useState<ProcessInfo[]>([])
  const [configOpen, setConfigOpen] = useState(false)

  useEffect(() => {
    setPortSubs([])
    setProcesses([])

    let cancelled = false

    async function hydrateFromSnapshot() {
      try {
        const info = await api.getSession(session.session_id)
        if (cancelled) return

        if (!info) {
          console.warn('Session info missing for', session.session_id)
          return
        }

        const procs = expectArray<ProcessInfo>(info.processes, 'processes', info)
          .map(p => ({ pid: p.pid, process_name: p.process_name }))
        if (procs.length > 0) setProcesses(procs)

        const ports = expectArray<PortSubscription>(info.port_subscriptions, 'port_subscriptions', info)
          .map(p => ({ port: p.port, mode: p.mode }))
        if (ports.length > 0) setPortSubs(ports)
      } catch (err) {
        const error = err instanceof Error ? err.message : String(err)
        console.warn('Failed to fetch session snapshot', err)
        emitUserBlocked('snapshot_fetch_failed', 'user_action', {
          session_id: session.session_id,
          error,
        })
      }
    }
    hydrateFromSnapshot()

    const eventSource = new EventSource(api.eventStreamUrl(session.session_id))

    eventSource.onmessage = (e) => {
      let event: MonitorEvent
      try {
        event = JSON.parse(e.data)
      } catch {
        return
      }

      switch (event.type) {
        case EventType.PortSubscription:
          setPortSubs(prev => (
            prev.some(p => p.port === event.port)
              ? prev
              : [...prev, { port: event.port, mode: event.mode }]
          ))
          break
        case EventType.LayerConnected:
          setProcesses(prev => (
            prev.some(p => p.pid === event.pid)
              ? prev
              : [...prev, { pid: event.pid, process_name: event.process_name }]
          ))
          break
        case EventType.LayerDisconnected:
          setProcesses(prev => prev.filter(p => p.pid !== event.pid))
          break
        default:
          break
      }
    }

    eventSource.onerror = () => {
      eventSource.close()
    }

    return () => {
      cancelled = true
      eventSource.close()
    }
  }, [session.session_id])

  const mode = Array.from(new Set(portSubs.map((p) => p.mode))).join(' · ')

  return (
    <div className="h-full flex flex-col">
      <SessionHeader
        session={session}
        processes={processes}
        mode={mode || undefined}
        trailing={
          <div className="flex items-center gap-2 shrink-0">
            {session.is_operator && (
              <JoinChip
                joinKey={session.key}
                extensionState={extensionState}
                onJoin={onJoin}
                onLeave={onLeave}
              />
            )}
            <button
              className="inline-flex items-center gap-1.5 border border-foreground/60 bg-card rounded-full px-3 py-1 text-xs font-semibold hover:bg-muted/50 transition-colors whitespace-nowrap"
              onClick={() => setConfigOpen(true)}
            >
              <SlidersHorizontal className="h-3 w-3" />
              Config
            </button>
          </div>
        }
        onKill={onKill}
      />
      <div className="flex-1 min-h-0 flex flex-col p-4 gap-4 w-full">
        <div className="flex-1 min-h-0">
          <EventStream session={session} />
        </div>
      </div>

      {configOpen && (
        <ConfigModal
          session={session}
          portSubs={portSubs}
          processes={processes}
          onClose={() => setConfigOpen(false)}
        />
      )}
    </div>
  )
}
