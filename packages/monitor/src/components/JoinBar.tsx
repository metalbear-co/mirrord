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
  onJoin: () => Promise<{ ok: boolean; error?: string | undefined }>
  onLeave: () => Promise<{ ok: boolean; error?: string | undefined }>
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
      <div className="bg-card border-border flex items-center gap-3 rounded-lg border px-4 py-3">
        <LogIn className="text-muted-foreground h-4 w-4 shrink-0" />
        <div className="flex-1 text-xs leading-relaxed">
          {strings.joinBar.installPrefix}{' '}
          <a
            href={CHROME_WEB_STORE_URL}
            target="_blank"
            rel="noreferrer"
            className="text-primary font-semibold hover:underline"
          >
            {strings.joinBar.extensionLink}
          </a>{' '}
          {strings.joinBar.installSuffix}
        </div>
        <Button asChild variant="outline" size="sm">
          <a href={CHROME_WEB_STORE_URL} target="_blank" rel="noreferrer">
            {strings.joinBar.install} <ExternalLink className="ml-1 h-3 w-3" />
          </a>
        </Button>
      </div>
    )
  }

  if (!extensionState.supportsBridge) {
    return (
      <div className="bg-card border-border flex items-center gap-3 rounded-lg border px-4 py-3">
        <LogIn className="text-muted-foreground h-4 w-4 shrink-0" />
        <div className="flex-1 text-xs leading-relaxed">
          {strings.joinBar.legacyExtensionPrefix}{' '}
          <span className="font-mono font-semibold">{joinKey}</span>
          {strings.joinBar.legacyExtensionSuffix}
        </div>
      </div>
    )
  }

  if (isJoinedToThis) {
    return (
      <div className="bg-primary/10 border-primary/30 flex items-center gap-3 rounded-lg border px-4 py-3">
        <Check className="text-primary h-4 w-4 shrink-0" />
        <div className="flex-1 text-xs leading-relaxed">
          <span className="font-semibold">{strings.joinBar.joinedTitle}</span>{' '}
          {strings.joinBar.joinedBody}
        </div>
        <Button
          variant="outline"
          size="sm"
          onClick={() => void handleLeave()}
          disabled={busy}
          className="gap-1.5"
        >
          <LogOut className="h-3 w-3" />
          {strings.joinBar.leave}
        </Button>
      </div>
    )
  }

  return (
    <div className="bg-card border-border flex items-center gap-3 rounded-lg border px-4 py-3">
      <LinkIcon className="text-muted-foreground h-4 w-4 shrink-0" />
      <div className="flex-1 text-xs leading-relaxed">
        {strings.joinBar.joinPrompt}
        <span className="mx-1.5">
          <Badge variant="outline" className="text-caps font-mono">
            {joinKey}
          </Badge>
        </span>
        {strings.joinBar.joinPromptSuffix}
        {extensionState.joinedKey && (
          <div className="text-meta text-muted-foreground mt-1">
            {strings.joinBar.currentlyJoinedPrefix}{' '}
            <span className="font-mono">{extensionState.joinedKey}</span>
            {strings.joinBar.currentlyJoinedSuffix}
          </div>
        )}
      </div>
      <Button
        onClick={() => void handleJoin()}
        disabled={busy}
        size="sm"
        variant="outline"
        className="gap-1.5"
      >
        <LogIn className="h-3 w-3" />
        {busy ? 'Joining…' : 'Join'}
      </Button>
      {err && <div className="text-meta text-destructive ml-3">{err}</div>}
    </div>
  )
}
