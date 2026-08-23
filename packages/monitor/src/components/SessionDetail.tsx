import { useState, useEffect } from 'react'
import { Activity } from 'lucide-react'
import type {
  SessionInfo,
  MonitorEvent,
  PortSubscription,
  ProcessInfo,
} from '../types'
import { api } from '../api'
import { emitUserBlocked } from '../analytics'
import { EventType } from '../eventTypes'
import {
  expectArray,
  formatHostPort,
  NOT_SET,
  extractContainerFromTarget,
  extractHttpFilterSummary,
  extractQueueSplitsFromConfig,
  formatQueueSplits,
  totalQueueSplits,
} from '../utils'
import { useChaosRules, type ChaosRuleFields } from '../hooks/useChaosRules'
import EventStream from './EventStream'
import SessionHeader from './SessionHeader'
import MetadataStrip from './MetadataStrip'
import PortModeChip from './PortModeChip'
import JoinBar from './JoinBar'
import ResizableSplit from './ResizableSplit'
import Widget from './Widget'
import SidePane, { type SidePaneTab } from './SidePane'
import type { ChaosFormRequest } from './chaos/ChaosPane'
import type { ExtensionState } from '../extensionBridge'

const MAX_SEEN_HOSTS = 3

interface Props {
  session: SessionInfo
  onKill: () => void
  extensionState: ExtensionState
  onJoin: () => Promise<{ ok: boolean; error?: string | undefined }>
  onLeave: () => Promise<{ ok: boolean; error?: string | undefined }>
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
  const [seenHosts, setSeenHosts] = useState<string[]>([])
  const [paneTab, setPaneTab] = useState<SidePaneTab>('config')
  const [formRequest, setFormRequest] = useState<ChaosFormRequest | null>(null)
  const chaos = useChaosRules(session.session_id)

  useEffect(() => {
    setPortSubs([])
    setProcesses([])
    setSeenHosts([])
    setPaneTab('config')
    setFormRequest(null)

    let cancelled = false

    async function hydrateFromSnapshot() {
      try {
        const info = await api.getSession(session.session_id)
        if (cancelled) return

        if (!info) {
          console.warn('Session info missing for', session.session_id)
          return
        }

        const procs = expectArray<ProcessInfo>(
          info.processes,
          'processes',
          info,
        ).map((p) => ({
          pid: p.pid,
          process_name: p.process_name,
        }))
        if (procs.length > 0) setProcesses(procs)

        const ports = expectArray<PortSubscription>(
          info.port_subscriptions,
          'port_subscriptions',
          info,
        ).map((p) => ({ port: p.port, mode: p.mode }))
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
    void hydrateFromSnapshot()

    const eventSource = new EventSource(api.eventStreamUrl(session.session_id))

    eventSource.onmessage = (e) => {
      let event: MonitorEvent
      try {
        event = JSON.parse(e.data as string) as MonitorEvent
      } catch {
        return
      }

      switch (event.type) {
        case EventType.PortSubscription:
          setPortSubs((prev) =>
            prev.some((p) => p.port === event.port)
              ? prev
              : [...prev, { port: event.port, mode: event.mode }],
          )
          break
        case EventType.LayerConnected:
          setProcesses((prev) =>
            prev.some((p) => p.pid === event.pid)
              ? prev
              : [...prev, { pid: event.pid, process_name: event.process_name }],
          )
          break
        case EventType.LayerDisconnected:
          setProcesses((prev) => prev.filter((p) => p.pid !== event.pid))
          break
        case EventType.OutgoingConnection: {
          const host = formatHostPort(event.address, event.port)
          setSeenHosts((prev) =>
            prev.includes(host) || prev.length >= MAX_SEEN_HOSTS
              ? prev
              : [...prev, host],
          )
          break
        }
        case EventType.FileOp:
        case EventType.DnsQuery:
        case EventType.IncomingRequest:
        case EventType.EnvVar:
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

  async function breakRule(fields: ChaosRuleFields) {
    await chaos.createRule(fields)
    setPaneTab('chaos')
  }

  function breakMore(host: string) {
    setPaneTab('chaos')
    setFormRequest({ upstream: host, nonce: Date.now() })
  }

  function newRule() {
    setFormRequest({ upstream: '', nonce: Date.now() })
  }

  const eventsWidget = (
    <Widget
      title="Events"
      icon={<Activity className="h-3 w-3" />}
      className="h-full min-h-0"
    >
      <div className="flex h-full flex-col">
        <EventStream
          session={session}
          chaosRules={chaos.rules}
          onBreakRule={breakRule}
          onBreakMore={breakMore}
        />
      </div>
    </Widget>
  )

  const sidePane = (
    <SidePane
      session={session}
      tab={paneTab}
      onTabChange={setPaneTab}
      chaos={chaos}
      seenHosts={seenHosts}
      formRequest={formRequest}
      onNewRule={newRule}
    />
  )

  return (
    <div className="flex h-full flex-col">
      <SessionHeader
        session={session}
        processes={processes}
        onKill={onKill}
        chaosArmedCount={chaos.armedCount}
        chaosTotalHits={chaos.totalHits}
        onChaosClick={() => setPaneTab('chaos')}
      />
      <div className="flex min-h-0 w-full flex-1 flex-col gap-4 p-4">
        {session.is_operator && session.key && (
          <JoinBar
            joinKey={session.key}
            extensionState={extensionState}
            onJoin={onJoin}
            onLeave={onLeave}
          />
        )}

        <MetadataStrip items={metadataItems(session, portSubs)} />

        <div className="hidden min-h-0 flex-1 lg:block">
          <ResizableSplit
            storageKey={`session-monitor-split:${session.session_id}`}
            left={<div className="h-full pr-2">{eventsWidget}</div>}
            right={<div className="h-full pl-2">{sidePane}</div>}
          />
        </div>
        <div className="grid min-h-0 flex-1 grid-cols-1 gap-4 lg:hidden">
          {eventsWidget}
          {sidePane}
        </div>
      </div>
    </div>
  )
}

// Same 8 fields, same order, same labels as `OperatorSessionDetail`'s metadata strip — the two
// views describe a session identically regardless of whether it's your own or a teammate's.
function metadataItems(session: SessionInfo, portSubs: PortSubscription[]) {
  const modes = Array.from(new Set(portSubs.map((p) => p.mode)))
  const queueSplits = extractQueueSplitsFromConfig(session.config)
  const queueSplitsValue = !session.is_operator
    ? 'Requires higher tier'
    : queueSplits && totalQueueSplits(queueSplits) > 0
      ? formatQueueSplits(queueSplits)
      : NOT_SET

  return [
    {
      label: 'Key',
      value: session.key
        ? session.key_generated
          ? `${session.key} (auto-generated)`
          : session.key
        : NOT_SET,
    },
    { label: 'Session ID', value: session.session_id },
    {
      label: portSubs.length === 1 ? 'Port' : 'Ports',
      value:
        portSubs.length > 0 ? (
          <span className="inline-flex flex-wrap items-center gap-1.5">
            {portSubs.map((p) => (
              <PortModeChip
                key={`${p.mode}:${p.port}`}
                port={p.port}
                mode={p.mode}
              />
            ))}
          </span>
        ) : (
          NOT_SET
        ),
    },
    { label: 'Mode', value: modes.length > 0 ? modes.join(' · ') : NOT_SET },
    { label: 'Namespace', value: session.namespace ?? NOT_SET },
    {
      label: 'Container',
      value: extractContainerFromTarget(session.target) ?? NOT_SET,
    },
    {
      label: 'HTTP filter',
      value: extractHttpFilterSummary(session.config) ?? NOT_SET,
    },
    {
      // Queue splitting requires the mirrord Operator — a direct/OSS-only session can't do it at
      // all, which is a different situation from an operator-backed session simply not using it.
      label: 'Queue splits',
      value: queueSplitsValue,
    },
  ]
}
