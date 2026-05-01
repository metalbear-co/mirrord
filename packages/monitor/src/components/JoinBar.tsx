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
      <div className="flex items-center gap-3 px-4 py-3 rounded-lg bg-card border border-border">
        <LogIn className="h-4 w-4 text-muted-foreground shrink-0" />
        <div className="text-xs leading-relaxed flex-1">
          Install the{' '}
          <a
            href={CHROME_WEB_STORE_URL}
            target="_blank"
            rel="noreferrer"
            className="text-primary hover:underline font-semibold"
          >
            mirrord browser extension
          </a>{' '}
          to inject this session&apos;s matching header into your browser traffic
          and ride along.
        </div>
        <Button asChild variant="outline" size="sm">
          <a href={CHROME_WEB_STORE_URL} target="_blank" rel="noreferrer">
            Install <ExternalLink className="h-3 w-3 ml-1" />
          </a>
        </Button>
      </div>
    )
  }

  if (!extensionState.supportsBridge) {
    return (
      <div className="flex items-center gap-3 px-4 py-3 rounded-lg bg-card border border-border">
        <LogIn className="h-4 w-4 text-muted-foreground shrink-0" />
        <div className="text-xs leading-relaxed flex-1">
          Browser extension is installed but doesn&apos;t support one-click join
          yet. Open the extension popup and click Join on key{' '}
          <span className="font-mono font-semibold">{joinKey}</span>, or update
          the extension.
        </div>
      </div>
    )
  }

  if (isJoinedToThis) {
    return (
      <div className="flex items-center gap-3 px-4 py-3 rounded-lg bg-primary/10 border border-primary/30">
        <Check className="h-4 w-4 text-primary shrink-0" />
        <div className="text-xs leading-relaxed flex-1">
          <span className="font-semibold">You&apos;re joined.</span> Outgoing
          browser requests now carry this session&apos;s matching header.
        </div>
        <Button
          variant="outline"
          size="sm"
          onClick={handleLeave}
          disabled={busy}
          className="gap-1.5"
        >
          <LogOut className="h-3 w-3" />
          Leave
        </Button>
      </div>
    )
  }

  return (
    <div className="flex items-center gap-3 px-4 py-3 rounded-lg bg-card border border-border">
      <LinkIcon className="h-4 w-4 text-muted-foreground shrink-0" />
      <div className="text-xs leading-relaxed flex-1">
        Join this session to route your browser traffic through
        <span className="mx-1.5">
          <Badge variant="outline" className="font-mono text-caps">
            {joinKey}
          </Badge>
        </span>
        — the extension will inject the matching header.
        {extensionState.joinedKey && (
          <div className="text-meta text-muted-foreground mt-1">
            Currently joined to{' '}
            <span className="font-mono">{extensionState.joinedKey}</span>;
            joining here will switch you over.
          </div>
        )}
      </div>
      <Button
        onClick={handleJoin}
        disabled={busy}
        size="sm"
        className="gap-1.5"
      >
        <LogIn className="h-3 w-3" />
        {busy ? 'Joining…' : 'Join'}
      </Button>
      {err && <div className="text-meta text-destructive ml-3">{err}</div>}
    </div>
  )
}
