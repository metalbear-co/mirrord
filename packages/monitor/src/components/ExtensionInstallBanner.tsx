import { useState } from 'react'
import { Button } from '@metalbear/ui'
import { Puzzle, ExternalLink, X } from 'lucide-react'
import { EXTENSION_INSTALL_URL } from '../extensionBridge'

const DISMISSED_KEY = 'mirrord_ui_install_banner_dismissed'

interface ExtensionInstallBannerProps {
  /** Show the banner — true once a ping has confirmed no extension is reachable. */
  show: boolean
}

/**
 * Nudge to install the browser extension, shown when the local UI can't reach one (no comms).
 * Links to the cross-browser landing page with UTM attribution. Dismissal is remembered so the
 * nudge doesn't reappear on every page load.
 */
export default function ExtensionInstallBanner({
  show,
}: ExtensionInstallBannerProps) {
  const [dismissed, setDismissed] = useState(
    () => localStorage.getItem(DISMISSED_KEY) === '1'
  )

  if (!show || dismissed) return null

  const dismiss = () => {
    localStorage.setItem(DISMISSED_KEY, '1')
    setDismissed(true)
  }

  return (
    <div className="flex items-center gap-3 px-4 py-2.5 bg-primary/5 border-b border-border">
      <Puzzle className="h-4 w-4 text-primary shrink-0" />
      <div className="text-xs leading-relaxed flex-1">
        Install the{' '}
        <a
          href={EXTENSION_INSTALL_URL}
          target="_blank"
          rel="noreferrer"
          className="text-primary hover:underline font-semibold"
        >
          mirrord browser extension
        </a>{' '}
        to join sessions and inject their matching header into your browser
        traffic.
      </div>
      <Button asChild variant="outline" size="sm">
        <a href={EXTENSION_INSTALL_URL} target="_blank" rel="noreferrer">
          Install <ExternalLink className="h-3 w-3 ml-1" />
        </a>
      </Button>
      <button
        type="button"
        aria-label="Dismiss"
        onClick={dismiss}
        className="text-muted-foreground hover:text-foreground"
      >
        <X className="h-4 w-4" />
      </button>
    </div>
  )
}
