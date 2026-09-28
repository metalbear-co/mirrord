import { useEffect, useState } from 'react'
import { createPortal } from 'react-dom'
import { EmptyState } from '@metalbear/ui'
import { selectKubeContext, useKubeContexts } from './contextStore'
import {
  MINIMUM_OPERATOR_VERSION,
  clearEvents,
  collectEvents,
  setRouting,
  useEvents,
} from './eventsStore'
import { ErrorBoundary } from './components/ErrorBoundary'
import { ContextPicker } from './components/ContextNamespacePicker'
import SubscribeEventsTable from './components/SubscribeEventsTable'
import { strings } from './strings'

interface EventsProps {
  /** Whether this is the tab on screen. Gates the top-bar controls only; collection is app-wide. */
  active: boolean
}

function Events({ active }: EventsProps) {
  const selection = useKubeContexts()
  const { rows, unavailable, streaming, routing } = useEvents()
  const [slot, setSlot] = useState<HTMLElement | null>(null)

  useEffect(() => {
    setSlot(document.getElementById('mirrord-topbar-slot'))
  }, [])

  useEffect(() => {
    collectEvents(selection)
  }, [selection])

  const picker =
    active &&
    slot &&
    createPortal(
      <ContextPicker
        contexts={selection.contexts}
        currentContext={selection.current}
        selectedContext={selection.selected}
        onSelectContext={selectKubeContext}
      />,
      slot,
    )

  if (unavailable) {
    return (
      <>
        {picker}
        <EmptyState
          title={
            unavailable.kind === 'unsupported'
              ? strings.subscribeEvents.operatorUnsupported(
                  unavailable.version,
                  MINIMUM_OPERATOR_VERSION,
                )
              : strings.subscribeEvents.operatorUnreachable
          }
          description={
            unavailable.kind === 'unreachable' && unavailable.reason !== ''
              ? unavailable.reason
              : undefined
          }
        />
      </>
    )
  }

  return (
    <>
      {picker}
      <SubscribeEventsTable
        rows={rows}
        streaming={streaming}
        routing={routing}
        onRoutingChange={setRouting}
        onClear={clearEvents}
      />
    </>
  )
}

export default function EventsTab(props: EventsProps) {
  return (
    <ErrorBoundary component="Events">
      <Events {...props} />
    </ErrorBoundary>
  )
}
