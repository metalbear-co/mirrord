import { useState } from 'react'
import { Badge, Button } from '@metalbear/ui'
import {
  Check,
  ExternalLink,
  Link as LinkIcon,
  LogIn,
  LogOut,
} from 'lucide-react'
import { CHROME_WEB_STORE_URL, type ExtensionState } from '../extensionBridge'
import { strings } from '../strings'

interface JoinBarProps {
  joinKey: string | null | undefined
  extensionState: ExtensionState
  onJoin: () => Promise<{ ok: boolean; error?: string }>
  onLeave: () => Promise<{ ok: boolean; error?: string }>
}

export default function JoinBar({
  joinKey,
  extensionState,
  onJoin,
  onLeave,
}: JoinBarProps) {
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState<string | null>(null)

  if (!joinKey) return null
  const isJoinedToThis = extensionState.joinedKey === joinKey

  const handleJoin = async () => {
    setBusy(true)
    setErr(null)
    const r = await onJoin()
    setBusy(false)
    if (!r.ok) setErr(r.error ?? 'Failed to join')
  }
  const handleLeave = async () => {
    setBusy(true)
    setErr(null)
    const r = await onLeave()
    setBusy(false)
    if (!r.ok) setErr(r.error ?? 'Failed to leave')
  }

  if (!extensionState.installed) {
    return (
      <div className="flex items-center gap-2.5 px-4 py-1.5 rounded-lg bg-card border border-border text-xs">
        <LogIn className="h-3.5 w-3.5 text-muted-foreground shrink-0" />
        <div className="flex-1 min-w-0 truncate">
          Install the{' '}
          <a
            href={CHROME_WEB_STORE_URL}
            target="_blank"
            rel="noreferrer"
            className="text-primary hover:underline font-semibold"
          >
            mirrord browser extension
          </a>{' '}
          to ride along with this session in your browser.
        </div>
        <Button asChild variant="outline" size="sm" className="h-6 px-2.5">
          <a href={CHROME_WEB_STORE_URL} target="_blank" rel="noreferrer">
            Install <ExternalLink className="h-3 w-3 ml-1" />
          </a>
        </Button>
      </div>
    )
  }

  if (!extensionState.supportsBridge) {
    return (
      <div className="flex items-center gap-2.5 px-4 py-1.5 rounded-lg bg-card border border-border text-xs">
        <LogIn className="h-3.5 w-3.5 text-muted-foreground shrink-0" />
        <div className="flex-1 min-w-0 truncate">
          {strings.joinBar.legacyExtensionPrefix}{' '}
          <span className="font-mono font-semibold">{joinKey}</span>
          {strings.joinBar.legacyExtensionSuffix}
        </div>
      </div>
    )
  }

  if (isJoinedToThis) {
    return (
      <div className="flex items-center gap-2.5 px-4 py-1.5 rounded-lg bg-primary/10 border border-primary/30 text-xs">
        <Check className="h-3.5 w-3.5 text-primary shrink-0" />
        <div className="flex-1 min-w-0 truncate">
          <span className="font-semibold">You&apos;re joined.</span> Browser
          requests carry this session&apos;s matching header.
        </div>
        <span className="font-mono bg-card border border-border rounded-md px-2 py-px whitespace-nowrap">
          mirrord-session={joinKey}
        </span>
        <Button
          variant="outline"
          size="sm"
          onClick={handleLeave}
          disabled={busy}
          className="gap-1.5 h-6 px-2.5"
        >
          <LogOut className="h-3 w-3" />
          Leave
        </Button>
      </div>
    )
  }

  return (
    <div className="flex items-center gap-2.5 px-4 py-1.5 rounded-lg bg-card border border-border text-xs">
      <LinkIcon className="h-3.5 w-3.5 text-muted-foreground shrink-0" />
      <div className="flex-1 min-w-0 truncate">
        Join to route your browser traffic through
        <span className="mx-1.5">
          <Badge variant="outline" className="font-mono text-caps">
            {joinKey}
          </Badge>
        </span>
        {extensionState.joinedKey && (
          <span className="text-muted-foreground">
            · currently joined to{' '}
            <span className="font-mono">{extensionState.joinedKey}</span>
          </span>
        )}
      </div>
      {err && <div className="text-meta text-destructive whitespace-nowrap">{err}</div>}
      <Button
        onClick={handleJoin}
        disabled={busy}
        size="sm"
        variant="outline"
        className="gap-1.5 h-6 px-2.5"
      >
        <LogIn className="h-3 w-3" />
        {busy ? 'Joining…' : 'Join'}
      </Button>
    </div>
  )
}
