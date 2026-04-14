import { useState, useEffect } from 'react'
import { Server, Settings, Activity } from 'lucide-react'
import type { SessionInfo, MonitorEvent, PortSubscription, ProcessInfo } from '../types'
import { api } from '../api'
import { strings } from '../strings'
import { EventType } from '../eventTypes'
import EventStream from './EventStream'
import SessionHeader from './SessionHeader'
import SessionTabs from './SessionTabs'
import OverviewTab from './OverviewTab'
import ConfigTab from './ConfigTab'
import {
  type DetailTab,
  type EventCounts,
  type TabDef,
  initialEventCounts,
} from './sessionDetailTypes'

interface Props {
  session: SessionInfo
  onKill: () => void
}

export default function SessionDetail({ session, onKill }: Props) {
  const [activeTab, setActiveTab] = useState<DetailTab>('overview')
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
        if (!Array.isArray(info.processes)) {
          console.warn('Session info has no `processes` array', info)
        } else if (info.processes.length > 0) {
          setProcesses(info.processes.map(p => ({ pid: p.pid, process_name: p.process_name })))
        }
        if (!Array.isArray(info.port_subscriptions)) {
          console.warn('Session info has no `port_subscriptions` array', info)
        } else if (info.port_subscriptions.length > 0) {
          setPortSubs(info.port_subscriptions.map(p => ({ port: p.port, mode: p.mode })))
        }
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

  const tabs: TabDef[] = [
    { id: 'overview', label: strings.session.overview, icon: Server },
    { id: 'events', label: strings.session.eventsTab, icon: Activity, count: eventCounts.total },
    { id: 'config', label: strings.session.configTab, icon: Settings },
  ]

  return (
    <div className="h-full flex flex-col">
      <SessionHeader session={session} processes={processes} onKill={onKill} />
      <SessionTabs tabs={tabs} activeTab={activeTab} onTabChange={setActiveTab} />
      <div className="flex-1 overflow-hidden">
        {activeTab === 'overview' && (
          <OverviewTab
            session={session}
            portSubs={portSubs}
            processes={processes}
            eventCounts={eventCounts}
            onSwitchTab={setActiveTab}
          />
        )}
        {activeTab === 'events' && <EventStream session={session} />}
        {activeTab === 'config' && <ConfigTab config={session.config} />}
      </div>
    </div>
  )
}
