import { useState, useEffect } from 'react'
import { Settings } from 'lucide-react'
import type { SessionInfo, MonitorEvent, PortSubscription, ProcessInfo } from '../types'
import { api } from '../api'
import { EventType } from '../eventTypes'
import { expectArray } from '../utils'
import EventStream from './EventStream'
import SessionHeader from './SessionHeader'
import LiveStatusStrip from './LiveStatusStrip'
import SessionIdentity from './SessionIdentity'
import PortSubscriptionsCard from './PortSubscriptionsCard'
import ProcessesCard from './ProcessesCard'
import ConfigTab from './ConfigTab'
import JoinBar from './JoinBar'
import Widget from './Widget'
import type { ExtensionState } from '../extensionBridge'
import { type EventCounts, initialEventCounts } from './sessionDetailTypes'

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
  const [eventCounts, setEventCounts] = useState<EventCounts>(initialEventCounts)

  useEffect(() => {
    setPortSubs([])
    setProcesses([])
    setEventCounts(initialEventCounts)

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
        console.warn('Failed to fetch session snapshot', err)
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

      setEventCounts(prev => ({
        ...prev,
        [event.type]: (prev[event.type as keyof EventCounts] as number || 0) + 1,
        total: prev.total + 1,
      }))

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

  return (
    <div className="h-full flex flex-col">
      <SessionHeader session={session} processes={processes} onKill={onKill} />
      <div className="flex-1 overflow-auto">
        <div className="grid grid-cols-2 gap-4 p-4 max-w-5xl mx-auto auto-rows-min">
          <div className="col-span-2">
            <LiveStatusStrip
              session={session}
              portSubs={portSubs}
              processes={processes}
              eventCounts={eventCounts}
            />
          </div>

          {session.is_operator && session.key && (
            <div className="col-span-2">
              <JoinBar
                joinKey={session.key}
                extensionState={extensionState}
                onJoin={onJoin}
                onLeave={onLeave}
              />
            </div>
          )}

          <SessionIdentity session={session} />
          <PortSubscriptionsCard portSubs={portSubs} />

          {processes.length > 0 && (
            <div className="col-span-2">
              <ProcessesCard processes={processes} />
            </div>
          )}

          <div className="col-span-2 h-[420px]">
            <div className="rounded-md border border-border bg-card overflow-hidden h-full flex flex-col">
              <EventStream session={session} />
            </div>
          </div>

          <div className="col-span-2">
            <Widget
              title="Config"
              icon={<Settings className="h-3 w-3" />}
              collapsible
              defaultOpen={false}
            >
              <ConfigTab config={session.config} />
            </Widget>
          </div>
        </div>
      </div>
    </div>
  )
}
