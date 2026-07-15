import { Button, cn } from '@metalbear/ui'
import { ChevronDown, Settings, User } from 'lucide-react'
import { useEffect, useRef, useState } from 'react'
import { createPortal } from 'react-dom'
import { strings } from '../strings'
import SettingsDialog from './SettingsDialog'
import ContextNamespacePicker from './ContextNamespacePicker'
import type { KubeContext } from '../types'

import type { ThemePref } from '../theme'

interface Props {
  active: boolean
  connected: boolean
  isDarkMode: boolean
  theme: ThemePref
  onThemeChange: (t: ThemePref) => void
  telemetryEnabled: boolean
  onTelemetryChange: (enabled: boolean) => void
  currentUser: string | null
  contexts: KubeContext[]
  currentContext: string | null
  selectedContext: string | null
  onSelectContext: (context: string | null) => void
  namespaces: string[]
  selectedNamespace: string | null
  onSelectNamespace: (namespace: string | null) => void
  namespacesLoading: boolean
  namespacesError: boolean
}

export default function AppHeader({
  active,
  connected,
  theme,
  onThemeChange,
  telemetryEnabled,
  onTelemetryChange,
  currentUser,
  contexts,
  currentContext,
  selectedContext,
  onSelectContext,
  namespaces,
  selectedNamespace,
  onSelectNamespace,
  namespacesLoading,
  namespacesError,
}: Props) {
  const [settingsOpen, setSettingsOpen] = useState(false)
  const [menuOpen, setMenuOpen] = useState(false)
  const menuRef = useRef<HTMLDivElement>(null)
  // The shared `mirrord-ui` shell owns the top bar and exposes a slot; the monitor's chrome
  // (kube context, namespace, account) renders into it rather than its own header row.
  const [slot, setSlot] = useState<HTMLElement | null>(null)

  useEffect(() => {
    setSlot(document.getElementById('mirrord-topbar-slot'))
  }, [])

  useEffect(() => {
    if (!menuOpen) return
    const onClick = (e: MouseEvent) => {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) {
        setMenuOpen(false)
      }
    }
    const onEsc = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setMenuOpen(false)
    }
    document.addEventListener('mousedown', onClick)
    document.addEventListener('keydown', onEsc)
    return () => {
      document.removeEventListener('mousedown', onClick)
      document.removeEventListener('keydown', onEsc)
    }
  }, [menuOpen])

  const controls = (
    <div className="flex items-center gap-2 min-w-0">
      <ContextNamespacePicker
        contexts={contexts}
        currentContext={currentContext}
        selectedContext={selectedContext}
        onSelectContext={onSelectContext}
        namespaces={namespaces}
        selectedNamespace={selectedNamespace}
        onSelectNamespace={onSelectNamespace}
        namespacesLoading={namespacesLoading}
        namespacesError={namespacesError}
      />

      <div ref={menuRef} className="relative">
        <button
          type="button"
          onClick={() => setMenuOpen((o) => !o)}
          title={currentUser ?? undefined}
          className="inline-flex items-center gap-1.5 rounded-full border border-border bg-muted/50 hover:bg-muted px-2.5 h-7 max-w-[240px] cursor-pointer transition-colors"
        >
          <span
            className={cn(
              'h-1.5 w-1.5 rounded-full shrink-0',
              connected ? 'bg-green-500' : 'bg-red-500'
            )}
            aria-label={connected ? strings.app.connected : strings.app.disconnected}
          />
          <User className="h-3 w-3 shrink-0 text-muted-foreground" />
          <span className="font-mono text-meta text-foreground truncate">
            {currentUser ?? '…'}
          </span>
          <ChevronDown className="h-3 w-3 shrink-0 text-muted-foreground" />
        </button>

        {menuOpen && (
          <div className="absolute right-0 top-full mt-1.5 z-50 min-w-[220px] rounded-lg border border-border bg-popover text-popover-foreground shadow-lg p-2 flex flex-col">
            {currentUser && (
              <div className="px-2 py-1.5">
                <div className="text-caps text-muted-foreground">Running as</div>
                <div className="font-mono text-meta text-foreground truncate" title={currentUser}>
                  {currentUser}
                </div>
              </div>
            )}

            <button
              type="button"
              onClick={() => {
                setMenuOpen(false)
                setSettingsOpen(true)
              }}
              className="flex items-center gap-2 px-2 py-1.5 rounded text-meta text-foreground hover:bg-muted transition-colors"
            >
              <Settings className="h-3.5 w-3.5 text-muted-foreground" />
              {strings.app.settings}
            </button>
          </div>
        )}
      </div>
    </div>
  )

  return (
    <>
      {active && slot && createPortal(controls, slot)}

      <SettingsDialog
        open={settingsOpen}
        onOpenChange={setSettingsOpen}
        theme={theme}
        onThemeChange={onThemeChange}
        telemetryEnabled={telemetryEnabled}
        onTelemetryChange={onTelemetryChange}
      />
    </>
  )
}
