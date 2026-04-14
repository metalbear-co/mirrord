import type { SessionInfo, PortSubscription, ProcessInfo } from '../types'
import LiveStatusStrip from './LiveStatusStrip'
import SessionIdentity from './SessionIdentity'
import PortSubscriptionsCard from './PortSubscriptionsCard'
import ProcessesCard from './ProcessesCard'
import type { DetailTab, EventCounts } from './sessionDetailTypes'

interface Props {
  session: SessionInfo
  portSubs: PortSubscription[]
  processes: ProcessInfo[]
  eventCounts: EventCounts
  onSwitchTab: (tab: DetailTab) => void
}

export default function OverviewTab({
  session,
  portSubs,
  processes,
  eventCounts,
  onSwitchTab,
}: Props) {
  return (
    <div className="overflow-auto h-full">
      <div className="p-4 space-y-4 max-w-3xl">
        <LiveStatusStrip
          session={session}
          portSubs={portSubs}
          processes={processes}
          eventCounts={eventCounts}
          onSwitchTab={onSwitchTab}
        />
        <SessionIdentity session={session} />
        <PortSubscriptionsCard portSubs={portSubs} />
        <ProcessesCard processes={processes} />
      </div>
    </div>
  )
}
