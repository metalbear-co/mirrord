import { useState } from 'react'
import { Activity, Check, Copy, Terminal, Users } from 'lucide-react'
import { Card, CardContent } from '@metalbear/ui'

const QUICK_START_CMD = 'mirrord exec -t deployment/<your-deployment> -- <your command>'
const PREVIEW_CMD =
  'mirrord preview start -t deployment/<your-deployment> -i <your-image> -k <your-key>'

function CopyableCommand({ cmd }: { cmd: string }) {
  const [copied, setCopied] = useState(false)
  return (
    <div
      role="button"
      tabIndex={0}
      onClick={() => {
        navigator.clipboard.writeText(cmd).catch(() => {})
        setCopied(true)
        setTimeout(() => setCopied(false), 1500)
      }}
      className="group flex items-center gap-2 rounded-md border border-border surface-inset px-3 py-2 font-mono text-meta text-foreground hover:border-primary/50 hover:surface-section transition-colors cursor-pointer"
    >
      <Terminal className="h-3 w-3 text-muted-foreground shrink-0" />
      <span className="truncate flex-1">{cmd}</span>
      {copied ? (
        <Check className="h-3 w-3 text-emerald-500 shrink-0" />
      ) : (
        <Copy className="h-3 w-3 text-muted-foreground shrink-0 opacity-0 group-hover:opacity-100 transition-opacity" />
      )}
    </div>
  )
}

export default function EmptySessionState() {
  return (
    <div className="h-full flex items-center justify-center p-6">
      <div className="max-w-lg w-full flex flex-col gap-5">
        <div className="flex flex-col items-center gap-3 text-center">
          <Activity className="h-7 w-7 text-muted-foreground/40" />
          <h2 className="text-base font-semibold text-foreground">
            No sessions yet
          </h2>
          <p className="text-xs text-muted-foreground max-w-sm">
            Start a mirrord session and it'll appear here. Pick something below
            to get going, or join a teammate's session from the right-side
            extension.
          </p>
        </div>

        <Card className="surface-section">
          <CardContent className="p-4 flex flex-col gap-3">
            <div className="flex items-center gap-2">
              <Terminal className="h-3 w-3 text-primary" />
              <span className="text-section text-foreground">
                Run a session
              </span>
            </div>
            <CopyableCommand cmd={QUICK_START_CMD} />
            <p className="text-meta text-muted-foreground">
              Targets a deployment, runs your local process in its context.
              Drop this into a terminal.
            </p>
          </CardContent>
        </Card>

        <Card className="surface-section">
          <CardContent className="p-4 flex flex-col gap-3">
            <div className="flex items-center gap-2">
              <Users className="h-3 w-3 text-emerald-500" />
              <span className="text-section text-foreground">
                Spin up a preview env
              </span>
            </div>
            <CopyableCommand cmd={PREVIEW_CMD} />
            <p className="text-meta text-muted-foreground">
              Spawns a long-lived preview pod from your image, shareable with
              teammates via a header key.
            </p>
          </CardContent>
        </Card>

        <p className="text-caps text-muted-foreground/60 text-center">
          Sessions started anywhere on this machine, plus teammate sessions
          from the operator, show up automatically.
        </p>
      </div>
    </div>
  )
}
