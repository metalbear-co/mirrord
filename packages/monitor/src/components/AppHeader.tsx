import { cn } from '@metalbear/ui'
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
    <div className="flex min-w-0 items-center gap-2">
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
          className="border-border bg-muted/50 hover:bg-muted inline-flex h-7 max-w-[240px] cursor-pointer items-center gap-1.5 rounded-full border px-2.5 transition-colors"
        >
          <span
            className={cn(
              'h-1.5 w-1.5 shrink-0 rounded-full',
              connected ? 'bg-green-500' : 'bg-red-500',
            )}
            aria-label={connected ? strings.app.connected : strings.app.disconnected}
          />
          <User className="text-muted-foreground h-3 w-3 shrink-0" />
          <span className="text-meta text-foreground truncate font-mono">{currentUser ?? '…'}</span>
          <ChevronDown className="text-muted-foreground h-3 w-3 shrink-0" />
        </button>

        {menuOpen && (
          <div className="border-border bg-popover text-popover-foreground absolute right-0 top-full z-50 mt-1.5 flex min-w-[220px] flex-col rounded-lg border p-2 shadow-lg">
            {currentUser && (
              <div className="px-2 py-1.5">
                <div className="text-caps text-muted-foreground">Running as</div>
                <div className="text-meta text-foreground truncate font-mono" title={currentUser}>
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
              className="text-meta text-foreground hover:bg-muted flex items-center gap-2 rounded px-2 py-1.5 transition-colors"
            >
              <Settings className="text-muted-foreground h-3.5 w-3.5" />
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
