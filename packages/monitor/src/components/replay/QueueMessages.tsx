import { memo, useMemo } from 'react'
import { Loader2, RotateCcw } from 'lucide-react'
import { Button } from '@metalbear/ui'
import type { KeptQueueMessage } from '../../types'
import type { UseQueueReplay } from '../../hooks/useQueueReplay'
import { strings } from '../../strings'

/** The queue or topic of a message, and its id: the SQS message id or Kafka `partition@offset`. */
function describe(message: KeptQueueMessage): { queue: string; id: string } {
  const sqs = message.event.queue_message
  if (sqs) return { queue: sqs.queue_name, id: sqs.message_id ?? '' }

  const kafka = message.event.kafka_message
  if (kafka) {
    return { queue: kafka.topic, id: `${kafka.partition}@${kafka.offset}` }
  }

  return { queue: '', id: '' }
}

function MessageRow({
  message,
  queue,
}: {
  message: KeptQueueMessage
  queue: UseQueueReplay
}) {
  const s = strings.replay.queue
  const { queue: name, id } = describe(message)
  const sending = queue.sendingId === message.id
  const error = queue.sendError?.id === message.id ? queue.sendError : null

  return (
    <div className="border-border flex flex-col gap-1 border-b px-3 py-2">
      <div className="flex items-center gap-2">
        <span className="text-body text-foreground min-w-0 flex-1 truncate font-mono">
          {name}
        </span>
        {message.replay_count > 0 && (
          <span className="text-meta text-primary flex shrink-0 items-center gap-1 font-semibold">
            <RotateCcw className="h-3 w-3" />
            {message.replay_count}
          </span>
        )}
        <span className="text-meta text-muted-foreground shrink-0 font-mono">
          {s.size(message.body_size)}
        </span>
        {message.not_replayable === undefined && (
          <Button
            size="sm"
            variant="outline"
            className="h-6 px-2"
            disabled={queue.sendingId !== null}
            onClick={() => void queue.send(message.id)}
          >
            {sending ? (
              <Loader2 className="mr-1 h-3 w-3 animate-spin" />
            ) : (
              <RotateCcw className="mr-1 h-3 w-3" />
            )}
            <span className="text-meta font-semibold">{s.replay}</span>
          </Button>
        )}
      </div>
      <div className="text-meta text-muted-foreground truncate font-mono">
        {id} {message.body_preview}
      </div>
      {message.not_replayable !== undefined && (
        <p className="text-meta text-muted-foreground">
          {s.cantReplay(message.not_replayable)}
        </p>
      )}
      {error && (
        <p className="text-meta text-destructive break-words">
          {s.replayFailed(error.detail)}
        </p>
      )}
    </div>
  )
}

function QueueMessages({ queue }: { queue: UseQueueReplay }) {
  const s = strings.replay.queue
  const newestFirst = useMemo(
    () => [...queue.messages].reverse(),
    [queue.messages],
  )

  return (
    <section className="border-border shrink-0 border-b">
      <div className="text-title text-foreground px-3 pb-1 pt-2">{s.title}</div>
      {queue.loadError ? (
        <p className="text-meta text-muted-foreground break-words px-3 pb-2">
          {s.loadFailed(queue.loadError)}
        </p>
      ) : newestFirst.length === 0 ? (
        <p className="text-meta text-muted-foreground px-3 pb-2 leading-relaxed">
          {s.empty}
        </p>
      ) : (
        <>
          <p className="text-meta text-muted-foreground px-3 pb-1 leading-relaxed">
            {s.replayHint}
          </p>
          <div className="max-h-64 overflow-y-auto">
            {newestFirst.map((message) => (
              <MessageRow key={message.id} message={message} queue={queue} />
            ))}
          </div>
        </>
      )}
    </section>
  )
}

export default memo(QueueMessages)
