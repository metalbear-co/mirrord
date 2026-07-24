import { Split } from 'lucide-react'
import { cn } from '@metalbear/ui'
import type {
  QueueSplitFilterInfo,
  QueueSplitQueueInfo,
  QueueSplitView,
} from '../../types'
import type { UseQueueSplits } from '../../hooks/useQueueSplits'
import { strings } from '../../strings'

const s = strings.queues

function phaseLabel(phase: string): string {
  if (phase === 'Ready') return s.phaseReady
  if (phase === 'Failed') return s.phaseFailed
  return s.phasePending
}

function phaseDotClass(phase: string): string {
  if (phase === 'Ready') return 'bg-emerald-500'
  if (phase === 'Failed') return 'bg-destructive'
  return 'bg-amber-500 animate-pulse'
}

function phaseTextClass(phase: string): string {
  if (phase === 'Ready') return 'text-emerald-600 dark:text-emerald-400'
  if (phase === 'Failed') return 'text-destructive'
  return 'text-amber-600 dark:text-amber-400'
}

function filterSummary(filter: QueueSplitFilterInfo | undefined): string[] {
  if (!filter) return []
  const parts = Object.entries(filter.messageFilter ?? {}).map(
    ([attr, pattern]) => `${attr} ~ ${pattern}`,
  )
  if (filter.jqFilter) parts.push(`${s.jqPrefix} ${filter.jqFilter}`)
  return parts
}

function QueueCard({
  queue,
  filter,
}: {
  queue: QueueSplitQueueInfo
  filter: QueueSplitFilterInfo | undefined
}) {
  const rows: { label: string; value: string }[] = []
  if (queue.queue) rows.push({ label: s.fieldQueue, value: queue.queue })
  if (queue.topic) rows.push({ label: s.fieldTopic, value: queue.topic })
  if (queue.consumerGroup)
    rows.push({ label: s.fieldGroup, value: queue.consumerGroup })
  if (queue.subscription)
    rows.push({ label: s.fieldSubscription, value: queue.subscription })
  const filters = filterSummary(filter)

  return (
    <div className="border-border bg-muted/30 rounded-lg border px-3 py-2.5">
      <div className="flex items-center gap-2">
        <span className="text-section text-foreground font-semibold">
          {queue.type}
        </span>
        <span className="text-meta text-muted-foreground truncate font-mono">
          {queue.id}
        </span>
      </div>
      <div className="mt-1.5 flex flex-col gap-0.5">
        {rows.length === 0 && (
          <span className="text-meta text-muted-foreground italic">
            {s.unresolved}
          </span>
        )}
        {rows.map((row) => (
          <div key={row.label} className="flex items-baseline gap-2">
            <span className="text-caps text-muted-foreground w-20 shrink-0">
              {row.label}
            </span>
            <span className="text-meta text-foreground truncate font-mono">
              {row.value}
            </span>
          </div>
        ))}
        {filters.map((line) => (
          <div key={line} className="flex items-baseline gap-2">
            <span className="text-caps text-muted-foreground w-20 shrink-0">
              {s.fieldFilter}
            </span>
            <span className="text-meta text-foreground truncate font-mono">
              {line}
            </span>
          </div>
        ))}
      </div>
    </div>
  )
}

function SplitSection({ split }: { split: QueueSplitView }) {
  const filtersById = new Map(
    (split.filters ?? []).map((filter) => [filter.id, filter]),
  )
  const queues: QueueSplitQueueInfo[] =
    split.queues && split.queues.length > 0
      ? split.queues
      : (split.filters ?? []).map((filter) => ({
          id: filter.id,
          type: filter.queueType,
        }))
  const pods = split.targetPods ?? []

  return (
    <div className="flex flex-col gap-2.5">
      <div className="flex items-center gap-2">
        <span
          className={cn(
            'h-2 w-2 shrink-0 rounded-full',
            phaseDotClass(split.phase),
          )}
        />
        <span
          className={
            'text-section font-semibold ' + phaseTextClass(split.phase)
          }
        >
          {phaseLabel(split.phase)}
        </span>
        <span className="text-meta text-muted-foreground ml-auto truncate font-mono">
          {split.name}
        </span>
      </div>
      {split.message && (
        <p
          className={
            'text-meta break-words leading-relaxed ' +
            (split.phase === 'Failed'
              ? 'text-destructive'
              : 'text-muted-foreground')
          }
        >
          {split.message}
        </p>
      )}
      {queues.map((queue) => (
        <QueueCard
          key={queue.id + (queue.topic ?? queue.queue ?? '')}
          queue={queue}
          filter={filtersById.get(queue.id)}
        />
      ))}
      {pods.length > 0 && (
        <div className="text-meta text-muted-foreground flex flex-wrap items-center gap-x-2 gap-y-0.5">
          <span className="text-caps">{s.podsLabel}</span>
          {pods.map((pod) => (
            <span key={pod.name} className="truncate font-mono">
              {pod.name}
              <span
                className={
                  pod.patched
                    ? 'text-emerald-600 dark:text-emerald-400'
                    : 'text-amber-600 dark:text-amber-400'
                }
              >
                {' ' + s.podPatched(pod.patched)}
              </span>
              <span
                className={
                  pod.ready
                    ? 'text-emerald-600 dark:text-emerald-400'
                    : 'text-amber-600 dark:text-amber-400'
                }
              >
                {' ' + s.podReady(pod.ready)}
              </span>
            </span>
          ))}
        </div>
      )}
    </div>
  )
}

export default function QueuesPane({ queues }: { queues: UseQueueSplits }) {
  const { state } = queues

  return (
    <div className="min-h-0 flex-1 overflow-y-auto">
      <div className="flex flex-col gap-4 p-3">
        {state.status === 'unavailable' && (
          <p className="text-meta text-destructive break-words px-0.5 pt-1">
            {s.loadFailed(state.reason)}
          </p>
        )}
        {state.status === 'ready' &&
          state.splits.map((split) => (
            <SplitSection key={split.name || split.session} split={split} />
          ))}
        {state.status === 'ready' && state.splits.length === 0 && (
          <div className="mx-auto max-w-xs px-3 py-8 text-center">
            <Split className="text-muted-foreground mx-auto mb-3 h-5 w-5" />
            <div className="text-title text-foreground mb-1.5">
              {s.emptyTitle}
            </div>
            <div className="text-meta text-muted-foreground mb-1.5 leading-relaxed">
              {s.emptyBody}
            </div>
            <div className="text-meta text-muted-foreground/70 mb-3 leading-relaxed">
              {s.emptyHint}
            </div>
            <a
              href={s.docsUrl}
              target="_blank"
              rel="noreferrer"
              className="text-meta text-primary font-semibold hover:underline"
            >
              {s.docsLabel}
            </a>
          </div>
        )}
      </div>
    </div>
  )
}
