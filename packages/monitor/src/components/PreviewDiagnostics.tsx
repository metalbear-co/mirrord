import { useCallback, useEffect, useRef, useState } from 'react'
import { Button } from '@metalbear/ui'
import { AlertTriangle, Info } from 'lucide-react'
import type {
  PreviewDetail,
  PreviewMessageSeverity,
  PreviewPodLogs,
} from '../types'
import { strings } from '../strings'
import { api } from '../api'
import MetadataStrip from './MetadataStrip'

const SEVERITY_STYLE: Record<PreviewMessageSeverity, string> = {
  failure: 'border-destructive/40 text-destructive',
  degraded: 'border-amber-500/40 text-amber-600 dark:text-amber-500',
  unknown: 'border-border text-muted-foreground',
}

interface PreviewDiagnosticsProps {
  previewId: string
  context: string | null
  namespace: string | null
}

/** What the pane knows about the preview, or why it knows nothing. */
type State =
  | { kind: 'loading' }
  | { kind: 'loaded'; detail: PreviewDetail }
  | { kind: 'gone' }
  | { kind: 'unavailable' }

/** Pod output state. */
type Logs =
  | { kind: 'unread' }
  | { kind: 'reading' }
  | { kind: 'read'; entries: PreviewPodLogs[]; at: Date }
  | { kind: 'failed'; at: Date }

/** The operator's account of one preview. */
export default function PreviewDiagnostics({
  previewId,
  context,
  namespace,
}: PreviewDiagnosticsProps) {
  const [state, setState] = useState<State>({ kind: 'loading' })
  const [logs, setLogs] = useState<Logs>({ kind: 'unread' })
  // Identifies the newest request; a reply carrying an older one is discarded.
  const request = useRef(0)

  useEffect(() => {
    let cancelled = false
    request.current += 1
    setState({ kind: 'loading' })
    setLogs({ kind: 'unread' })

    api
      .getPreviewDetail(previewId, context, namespace, false)
      .then((detail) => {
        if (cancelled) return
        setState(detail ? { kind: 'loaded', detail } : { kind: 'gone' })
      })
      .catch(() => {
        if (!cancelled) setState({ kind: 'unavailable' })
      })

    return () => {
      cancelled = true
    }
  }, [previewId, context, namespace])

  const readLogs = useCallback(() => {
    const current = (request.current += 1)
    const stale = () => request.current !== current

    setLogs({ kind: 'reading' })
    api
      .getPreviewDetail(previewId, context, namespace, true)
      .then((detail) => {
        if (stale()) return

        if (!detail || detail.logsError) {
          setLogs({ kind: 'failed', at: new Date() })
          return
        }

        setState({ kind: 'loaded', detail })
        setLogs({ kind: 'read', entries: detail.logs ?? [], at: new Date() })
      })
      .catch(() => {
        if (!stale()) setLogs({ kind: 'failed', at: new Date() })
      })
  }, [previewId, context, namespace])

  if (state.kind === 'loading') return null

  if (state.kind === 'gone' || state.kind === 'unavailable') {
    return (
      <p className="text-meta text-muted-foreground">
        {state.kind === 'gone'
          ? strings.previewDetail.gone
          : strings.previewDetail.unavailable}
      </p>
    )
  }

  const { detail } = state
  const clusters = Object.entries(detail.clusters ?? {})
  const readAt = logs.kind === 'read' || logs.kind === 'failed' ? logs.at : null

  return (
    <div className="flex min-h-0 flex-col gap-4">
      {detail.message && (
        <div
          className={`flex items-start gap-2 rounded-md border px-3 py-2 ${SEVERITY_STYLE[detail.message.severity]}`}
        >
          {detail.message.severity === 'failure' ? (
            <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0" />
          ) : (
            <Info className="mt-0.5 h-3.5 w-3.5 shrink-0" />
          )}
          <span className="text-body break-words">{detail.message.text}</span>
        </div>
      )}

      <MetadataStrip
        items={[
          { label: strings.previewDetail.image, value: detail.image || '—' },
          ...(clusters.length > 0
            ? [
                {
                  label: strings.previewDetail.clusters,
                  value: (
                    <span className="inline-flex flex-wrap items-center gap-1.5">
                      {clusters.map(([cluster, clusterPhase]) => (
                        <span
                          key={cluster}
                          className="border-border bg-card/40 text-meta inline-flex items-center gap-1.5 rounded-full border px-2 py-0.5 font-mono"
                        >
                          <span className="text-foreground font-medium">
                            {cluster}
                          </span>
                          <span className="text-muted-foreground">
                            {clusterPhase}
                          </span>
                        </span>
                      ))}
                    </span>
                  ),
                },
              ]
            : []),
        ]}
      />

      <section className="flex min-h-0 flex-col gap-2">
        <div className="flex items-center gap-2">
          <h3 className="text-caps text-muted-foreground">
            {strings.previewDetail.logsHeading}
          </h3>
          {readAt && (
            <span
              className="text-meta text-muted-foreground/70 tabular-nums"
              title={readAt.toISOString()}
            >
              {readAt.toLocaleTimeString()}
            </span>
          )}
          <Button
            variant="outline"
            size="sm"
            className="ml-auto h-6"
            disabled={logs.kind === 'reading'}
            onClick={readLogs}
          >
            {logs.kind === 'reading'
              ? strings.previewDetail.loadingLogs
              : logs.kind === 'unread'
                ? strings.previewDetail.loadLogs
                : strings.previewDetail.reloadLogs}
          </Button>
        </div>

        {logs.kind === 'unread' || logs.kind === 'reading' ? (
          <p className="text-meta text-muted-foreground">
            {logs.kind === 'reading'
              ? strings.previewDetail.loadingLogs
              : strings.previewDetail.logsNotLoaded}
          </p>
        ) : logs.kind === 'failed' ? (
          <p className="text-meta text-muted-foreground">
            {strings.previewDetail.logsUnavailable}
          </p>
        ) : logs.entries.length === 0 ? (
          <p className="text-meta text-muted-foreground">
            {strings.previewDetail.noLogs}
          </p>
        ) : (
          <div className="flex min-h-0 flex-col gap-3 overflow-auto">
            {logs.entries.map((pod) => (
              <div
                key={`${pod.cluster ?? ''}/${pod.pod}/${pod.container}`}
                className="border-border overflow-hidden rounded-md border"
              >
                <div className="surface-inset text-meta text-muted-foreground border-border flex flex-wrap items-center gap-x-2 border-b px-2 py-1 font-mono">
                  {pod.cluster && (
                    <span className="text-foreground">{pod.cluster}</span>
                  )}
                  <span>{pod.pod}</span>
                  <span className="text-muted-foreground/70">
                    {pod.container}
                  </span>
                </div>
                <pre className="text-meta max-h-64 overflow-auto px-2 py-1.5 font-mono whitespace-pre-wrap">
                  {pod.logs}
                </pre>
              </div>
            ))}
          </div>
        )}
      </section>
    </div>
  )
}
