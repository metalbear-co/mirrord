import { useState } from 'react'
import { Activity, Check, Copy, Terminal, Users } from 'lucide-react'
import { Card, CardContent } from '@metalbear/ui'

const QUICK_START_CMD = 'mirrord exec -t deployment/<your-deployment> -- <your command>'
const PREVIEW_CMD =
  'mirrord preview start -t deployment/<your-deployment> -i <your-image> -k <your-key>'
const COPY_FEEDBACK_MS = 1500

function CopyableCommand({ cmd }: { cmd: string }) {
  const [copied, setCopied] = useState(false)
  const copy = () => {
    navigator.clipboard.writeText(cmd).catch(() => undefined)
    setCopied(true)
    setTimeout(() => setCopied(false), COPY_FEEDBACK_MS)
  }
  return (
    <div
      role="button"
      tabIndex={0}
      onClick={copy}
      onKeyDown={(e) => {
        if (e.key === 'Enter' || e.key === ' ') {
          e.preventDefault()
          copy()
        }
      }}
      className="border-border surface-inset text-meta text-foreground hover:border-primary/50 hover:surface-section group flex cursor-pointer items-center gap-2 rounded-md border px-3 py-2 font-mono transition-colors"
    >
      <Terminal className="text-muted-foreground h-3 w-3 shrink-0" />
      <span className="flex-1 truncate">{cmd}</span>
      {copied ? (
        <Check className="h-3 w-3 shrink-0 text-emerald-500" />
      ) : (
        <Copy className="text-muted-foreground h-3 w-3 shrink-0 opacity-0 transition-opacity group-hover:opacity-100" />
      )}
    </div>
  )
}

export default function EmptySessionState() {
  return (
    <div className="flex h-full items-center justify-center p-6">
      <div className="flex w-full max-w-lg flex-col gap-5">
        <div className="flex flex-col items-center gap-3 text-center">
          <Activity className="text-muted-foreground/40 h-7 w-7" />
          <h2 className="text-foreground text-base font-semibold">No sessions yet</h2>
          <p className="text-muted-foreground max-w-sm text-xs">
            Start a mirrord session and it'll appear here. Pick something below to get going, or
            join a teammate's session from the right-side extension.
          </p>
        </div>

        <Card className="surface-section">
          <CardContent className="flex flex-col gap-3 p-4">
            <div className="flex items-center gap-2">
              <Terminal className="text-primary h-3 w-3" />
              <span className="text-section text-foreground">Run a session</span>
            </div>
            <CopyableCommand cmd={QUICK_START_CMD} />
            <p className="text-meta text-muted-foreground">
              Targets a deployment, runs your local process in its context. Drop this into a
              terminal.
            </p>
          </CardContent>
        </Card>

        <Card className="surface-section">
          <CardContent className="flex flex-col gap-3 p-4">
            <div className="flex items-center gap-2">
              <Users className="h-3 w-3 text-emerald-500" />
              <span className="text-section text-foreground">Spin up a preview env</span>
            </div>
            <CopyableCommand cmd={PREVIEW_CMD} />
            <p className="text-meta text-muted-foreground">
              Spawns a long-lived preview pod from your image, shareable with teammates via a header
              key.
            </p>
          </CardContent>
        </Card>

        <p className="text-caps text-muted-foreground/60 text-center">
          Sessions started anywhere on this machine, plus teammate sessions from the operator, show
          up automatically.
        </p>
      </div>
    </div>
  )
}
