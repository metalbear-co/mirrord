import { memo, useMemo, useState } from 'react'
import { ChevronLeft, Loader2, RotateCcw } from 'lucide-react'
import { Badge, Button } from '@metalbear/ui'
import type {
  CapturedBody,
  CapturedRequest,
  CapturedRequestSummary,
} from '../../types'
import type { UseReplay } from '../../hooks/useReplay'
import { strings } from '../../strings'
import JsonHighlight from '../JsonHighlight'
import { TONES, statusTone } from '../SubscribeEventsTable'

function StatusChip({ status }: { status: number | null }) {
  if (status === null) {
    return (
      <span className="text-meta text-muted-foreground font-mono">
        {strings.replay.noStatus}
      </span>
    )
  }

  return (
    <Badge variant="outline" className={TONES[statusTone(String(status))]}>
      {status}
    </Badge>
  )
}

const JSON_STRING_OR_WHITESPACE_RE = /("(?:\\.|[^"\\])*")|\s+/g

/** Removes the whitespace between the tokens of a JSON text. */
function withoutJsonWhitespace(json: string): string {
  return json.replace(
    JSON_STRING_OR_WHITESPACE_RE,
    (_match, string: string | undefined) => string ?? '',
  )
}

// The embeddings, overrides and isolates of the Unicode bidirectional algorithm.
const DIRECTION_CHANGES = /[\u202a-\u202e\u2066-\u2069]/g
const HEX = 16

/**
 * Returns `text` with the characters that change the direction of text escaped, as
 * `mirrord replay` prints them, because they would show the text in another order than the bytes
 * that a replay sends.
 */
function visible(text: string): string {
  return text.replace(
    DIRECTION_CHANGES,
    (c) => `\\u{${c.codePointAt(0)?.toString(HEX)}}`,
  )
}

function Body({ body }: { body: CapturedBody }) {
  const s = strings.replay
  if (body.size === 0) {
    return (
      <p className="text-meta text-muted-foreground">
        {body.truncated ? s.noBodyInTime : s.emptyBody}
      </p>
    )
  }

  // Formatted only when that can't change what is shown: parsing rounds large numbers and merges
  // repeated keys.
  // A body with direction changes is shown as text, where they are escaped.
  const text = visible(body.text)
  let parsed: unknown
  try {
    const value: unknown = JSON.parse(body.text)
    if (
      text === body.text &&
      JSON.stringify(value) === withoutJsonWhitespace(body.text)
    ) {
      parsed = value
    }
  } catch {
    parsed = undefined
  }

  return (
    <div className="border-border bg-muted/40 rounded border p-2">
      {parsed === undefined ? (
        <pre className="text-meta whitespace-pre-wrap break-all font-mono leading-relaxed">
          {text}
        </pre>
      ) : (
        <JsonHighlight value={parsed} />
      )}
      {body.truncated && (
        <p className="text-meta text-muted-foreground mt-1.5">{s.truncated}</p>
      )}
    </div>
  )
}

// Parsing and highlighting a body of 32 KiB takes several milliseconds, and the request around it
// renders again on every poll.
const MemoBody = memo(Body)

function Headers({ headers }: { headers: [string, string][] }) {
  return (
    <div className="border-border bg-muted/40 text-meta rounded border p-2 font-mono leading-relaxed">
      {headers.map(([name, value], index) => (
        // A header name can repeat, so the position is part of the key.
        // eslint-disable-next-line react/no-array-index-key
        <div key={`${name}-${index}`} className="break-all">
          <span className="text-muted-foreground">{visible(name)}:</span>{' '}
          {visible(value)}
        </div>
      ))}
    </div>
  )
}

function RequestRow({
  request,
  onSelect,
}: {
  request: CapturedRequestSummary
  onSelect: (id: number) => void
}) {
  const s = strings.replay
  return (
    <button
      type="button"
      onClick={() => onSelect(request.id)}
      className="border-border hover:bg-muted/50 flex w-full items-center gap-2 border-b px-3 py-2 text-left"
    >
      <span className="text-meta text-muted-foreground w-12 shrink-0 font-mono font-semibold">
        {request.method}
      </span>
      <span className="text-body text-foreground min-w-0 flex-1 truncate font-mono">
        {visible(request.path)}
      </span>
      {request.replay_count > 0 && (
        <span className="text-meta text-primary flex shrink-0 items-center gap-1 font-semibold">
          <RotateCcw className="h-3 w-3" />
          {request.replay_count}
        </span>
      )}
      <StatusChip status={request.status} />
      <span className="text-meta text-muted-foreground w-14 shrink-0 text-right font-mono">
        {request.duration_ms === null ? '' : s.duration(request.duration_ms)}
      </span>
    </button>
  )
}

const MemoRequestRow = memo(RequestRow)

function RequestDetail({
  request,
  replay,
}: {
  request: CapturedRequest
  replay: UseReplay
}) {
  const s = strings.replay
  const [showHeaders, setShowHeaders] = useState(false)

  // A pick holds until the next replay, which shows its own response.
  const [picked, setPicked] = useState<{
    number: number
    replayCount: number
  } | null>(null)
  const pickedNumber =
    picked?.replayCount === request.replay_count ? picked.number : null

  const latestRun = request.replays[request.replays.length - 1]
  const run =
    request.replays.find((candidate) => candidate.number === pickedNumber) ??
    latestRun

  return (
    <div className="flex flex-col gap-3 p-3">
      <div>
        <div className="text-body text-foreground break-all font-mono font-semibold">
          {request.method} {visible(request.path)}
        </div>
        <div className="text-meta text-muted-foreground mt-0.5">
          {s.receivedLine(
            new Date(request.captured_at_ms).toLocaleTimeString(),
            request.mode,
            request.port,
          )}
        </div>
      </div>

      {request.not_replayable === null ? (
        <div>
          <Button
            size="sm"
            className="h-7"
            disabled={replay.sending}
            onClick={() => void replay.send()}
          >
            {replay.sending ? (
              <Loader2 className="mr-1.5 h-3 w-3 animate-spin" />
            ) : (
              <RotateCcw className="mr-1.5 h-3 w-3" />
            )}
            <span className="text-meta font-semibold">
              {request.replay_count > 0 ? s.replayAgain : s.replay}
            </span>
          </Button>
          <p className="text-meta text-muted-foreground mt-1.5 leading-relaxed">
            {s.replayHint}
          </p>
        </div>
      ) : (
        <p className="text-meta text-muted-foreground leading-relaxed">
          {s.cantReplay(s.notReplayable(request.not_replayable))}
        </p>
      )}

      {replay.sendError && (
        <p className="text-meta text-destructive break-words">
          {s.replayFailed(replay.sendError)}
        </p>
      )}

      <section className="flex flex-col gap-1.5">
        <div className="flex items-center gap-2">
          <span className="text-title text-foreground">{s.request}</span>
          <button
            type="button"
            className="text-meta text-muted-foreground hover:text-foreground ml-auto underline-offset-2 hover:underline"
            onClick={() => setShowHeaders((shown) => !shown)}
          >
            {showHeaders ? s.bodyLabel : s.headersLabel(request.headers.length)}
          </button>
        </div>
        {showHeaders ? (
          <Headers headers={request.headers} />
        ) : (
          <MemoBody body={request.body} />
        )}
      </section>

      <section className="flex flex-col gap-1.5">
        <span className="text-title text-foreground">{s.responses}</span>
        <div className="border-border rounded border">
          <div className="text-meta flex items-center gap-2 px-2.5 py-1.5">
            <span className="text-foreground flex-1">{s.original}</span>
            <StatusChip status={request.status} />
            <span className="text-muted-foreground w-14 text-right font-mono">
              {request.duration_ms === null
                ? ''
                : s.duration(request.duration_ms)}
            </span>
          </div>
          {request.replays.map((candidate) => (
            <button
              key={candidate.number}
              type="button"
              onClick={() =>
                setPicked({
                  number: candidate.number,
                  replayCount: request.replay_count,
                })
              }
              className={
                'border-border text-meta flex w-full items-center gap-2 border-t px-2.5 py-1.5 text-left ' +
                (candidate.number === run?.number ? 'bg-primary/10' : '')
              }
            >
              <span className="text-foreground flex-1">
                {s.replayRun(candidate.number)}
              </span>
              <StatusChip status={candidate.response.status} />
              <span className="text-muted-foreground w-14 text-right font-mono">
                {s.duration(candidate.response.duration_ms)}
              </span>
            </button>
          ))}
        </div>
      </section>

      {run && (
        <section className="flex flex-col gap-1.5">
          <span className="text-title text-foreground">
            {s.responseOf(run.number)}
          </span>
          <MemoBody body={run.body} />
        </section>
      )}
    </div>
  )
}

function ReplayPane({ replay }: { replay: UseReplay }) {
  const s = strings.replay
  const newestFirst = useMemo(
    () => [...replay.requests].reverse(),
    [replay.requests],
  )

  if (replay.selectedId !== null) {
    return (
      <div className="flex min-h-0 flex-1 flex-col">
        <button
          type="button"
          onClick={() => replay.select(null)}
          className="border-border text-meta text-muted-foreground hover:text-foreground flex shrink-0 items-center gap-1 border-b px-3 py-1.5 text-left"
        >
          <ChevronLeft className="h-3 w-3" />
          {s.backToList}
        </button>
        <div className="min-h-0 flex-1 overflow-y-auto">
          {replay.selected ? (
            <RequestDetail
              key={replay.selected.id}
              request={replay.selected}
              replay={replay}
            />
          ) : (
            <p
              className={
                'text-meta break-words p-3 ' +
                (replay.selectedError
                  ? 'text-destructive'
                  : 'text-muted-foreground')
              }
            >
              {replay.selectedError
                ? s.requestFailed(replay.selectedError)
                : s.loadingRequest}
            </p>
          )}
        </div>
      </div>
    )
  }

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      {replay.loadError && (
        <p className="text-meta text-destructive shrink-0 px-3.5 pt-2">
          {s.loadFailed}
        </p>
      )}
      <div className="min-h-0 flex-1 overflow-y-auto">
        {newestFirst.map((request) => (
          <MemoRequestRow
            key={request.id}
            request={request}
            onSelect={replay.select}
          />
        ))}
        {replay.requests.length === 0 && !replay.loadError && (
          <div className="mx-auto max-w-xs px-3 py-8 text-center">
            <RotateCcw className="text-muted-foreground mx-auto mb-3 h-5 w-5" />
            <div className="text-title text-foreground mb-1.5">
              {replay.unsupported ? s.unsupportedTitle : s.emptyTitle}
            </div>
            <div className="text-meta text-muted-foreground leading-relaxed">
              {replay.unsupported ? s.unsupportedHint : s.emptyHint}
            </div>
          </div>
        )}
      </div>
      {!replay.unsupported && (
        <div className="border-border text-meta text-muted-foreground/70 shrink-0 border-t px-3.5 py-2 text-center">
          {s.footerNote}
        </div>
      )}
    </div>
  )
}

export default memo(ReplayPane)
