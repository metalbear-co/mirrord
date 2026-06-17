import { useEffect, useState } from 'react'
import { CheckCircle2, X } from 'lucide-react'
import {
  EXTENSION_CONFIGURED_EVENT,
  isExtensionConfigured,
} from '../extensionConfigure'

const AUTO_DISMISS_MS = 6000

/**
 * Confirmation banner shown once the browser extension acknowledges that this `mirrord ui`
 * page handed it the backend + token. Self-contained: listens for the configure event (and
 * checks the module flag on mount, in case the ack landed before render), auto-dismisses, and
 * can be closed manually.
 */
export default function ExtensionConfiguredToast() {
  const [visible, setVisible] = useState(false)

  useEffect(() => {
    const show = () => setVisible(true)
    if (isExtensionConfigured()) show()
    window.addEventListener(EXTENSION_CONFIGURED_EVENT, show)
    return () => window.removeEventListener(EXTENSION_CONFIGURED_EVENT, show)
  }, [])

  useEffect(() => {
    if (!visible) return
    const timer = setTimeout(() => setVisible(false), AUTO_DISMISS_MS)
    return () => clearTimeout(timer)
  }, [visible])

  if (!visible) return null

  return (
    <div
      role="status"
      className="fixed bottom-4 right-4 z-50 flex items-center gap-2 rounded-md border border-border bg-card px-4 py-2 text-sm text-foreground shadow-lg"
    >
      <CheckCircle2 className="h-4 w-4 text-green-500" />
      <span>Browser extension configured</span>
      <button
        type="button"
        aria-label="Dismiss"
        onClick={() => setVisible(false)}
        className="ml-2 text-muted-foreground hover:text-foreground"
      >
        <X className="h-4 w-4" />
      </button>
    </div>
  )
}
