import { useState } from 'react'
import { cn } from '@metalbear/ui'
import { Check, ExternalLink, LogIn } from 'lucide-react'
import { CHROME_WEB_STORE_URL, type ExtensionState } from '../extensionBridge'
import { strings } from '../strings'

interface JoinChipProps {
  joinKey: string | null | undefined
  extensionState: ExtensionState
  onJoin: () => Promise<{ ok: boolean; error?: string }>
  onLeave: () => Promise<{ ok: boolean; error?: string }>
}

const CHIP =
  'inline-flex items-center gap-1.5 border rounded-full px-3 py-1 text-xs font-semibold whitespace-nowrap transition-colors'

// Compact join control that lives in the session metadata strip next to the Config chip; the
// old full-width banner spent a whole row on one action.
export default function JoinChip({ joinKey, extensionState, onJoin, onLeave }: JoinChipProps) {
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState<string | null>(null)

  if (!joinKey) return null
  const isJoinedToThis = extensionState.joinedKey === joinKey

  if (!extensionState.installed) {
    return (
      <a
        href={CHROME_WEB_STORE_URL}
        target="_blank"
        rel="noreferrer"
        className={cn(CHIP, 'border-border bg-card text-muted-foreground hover:text-foreground')}
        title="Install the mirrord browser extension to ride along with this session in your browser"
      >
        <ExternalLink className="h-3 w-3" />
        Get browser extension
      </a>
    )
  }

  if (!extensionState.supportsBridge) {
    return (
      <span
        className={cn(CHIP, 'border-border bg-card text-muted-foreground font-normal')}
        title={`${strings.joinBar.legacyExtensionPrefix} ${joinKey}${strings.joinBar.legacyExtensionSuffix}`}
      >
        <LogIn className="h-3 w-3" />
        Join via extension
      </span>
    )
  }

  const run = async (action: () => Promise<{ ok: boolean; error?: string }>, fallback: string) => {
    setBusy(true)
    setErr(null)
    const r = await action()
    setBusy(false)
    if (!r.ok) setErr(r.error ?? fallback)
  }

  return (
    <>
      {isJoinedToThis ? (
        <button
          className={cn(CHIP, 'border-primary/40 bg-primary/10 text-foreground hover:bg-primary/20')}
          onClick={() => run(onLeave, 'Failed to leave')}
          disabled={busy}
          title={`Browser requests carry mirrord-session=${joinKey}. Click to leave.`}
        >
          <Check className="h-3 w-3 text-primary" />
          Joined <span className="font-mono font-normal">{joinKey}</span>
        </button>
      ) : (
        <button
          className={cn(CHIP, 'border-foreground/60 bg-card hover:bg-muted/50')}
          onClick={() => run(onJoin, 'Failed to join')}
          disabled={busy}
          title={
            extensionState.joinedKey
              ? `Route your browser traffic through this session. Currently joined to ${extensionState.joinedKey}; joining here switches you over.`
              : 'Route your browser traffic through this session via the extension'
          }
        >
          <LogIn className="h-3 w-3" />
          {busy ? 'Joining…' : 'Join browser'}
        </button>
      )}
      {err && <span className="text-meta text-destructive whitespace-nowrap">{err}</span>}
    </>
  )
}
