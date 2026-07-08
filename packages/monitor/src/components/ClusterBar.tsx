import { useEffect, useRef, useState } from 'react'
import { cn } from '@metalbear/ui'
import { Check, ChevronDown, Layers, Server } from 'lucide-react'
import type { KubeContext } from '../types'

interface DropdownOption {
  value: string | null
  label: string
  hint?: string
}

interface DropdownProps {
  icon: React.ReactNode
  label: string
  value: string
  options: DropdownOption[]
  selected: string | null
  onSelect: (value: string | null) => void
  disabled?: boolean
  emptyLabel?: string
}

function Dropdown({
  icon,
  label,
  value,
  options,
  selected,
  onSelect,
  disabled,
  emptyLabel,
}: DropdownProps) {
  const [open, setOpen] = useState(false)
  const ref = useRef<HTMLDivElement>(null)

  useEffect(() => {
    if (!open) return
    const onClick = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false)
    }
    const onEsc = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setOpen(false)
    }
    document.addEventListener('mousedown', onClick)
    document.addEventListener('keydown', onEsc)
    return () => {
      document.removeEventListener('mousedown', onClick)
      document.removeEventListener('keydown', onEsc)
    }
  }, [open])

  return (
    <div ref={ref} className="relative min-w-0">
      <button
        type="button"
        disabled={disabled}
        onClick={() => setOpen((o) => !o)}
        title={`${label}: ${value}`}
        className={cn(
          'inline-flex items-center gap-1.5 rounded-md border border-border bg-muted/40 hover:bg-muted px-2.5 h-7 max-w-[280px] transition-colors',
          disabled ? 'opacity-60 cursor-not-allowed' : 'cursor-pointer'
        )}
      >
        <span className="text-muted-foreground shrink-0">{icon}</span>
        <span className="text-caps text-muted-foreground shrink-0">{label}</span>
        <span className="text-meta text-foreground font-medium truncate">{value}</span>
        <ChevronDown className="h-3 w-3 shrink-0 text-muted-foreground" />
      </button>

      {open && (
        <div className="absolute left-0 top-full mt-1.5 z-50 min-w-[220px] max-w-[360px] max-h-[320px] overflow-y-auto rounded-lg border border-border bg-popover text-popover-foreground shadow-lg p-1 flex flex-col">
          {options.length === 0 ? (
            <div className="px-2 py-2 text-meta text-muted-foreground">
              {emptyLabel ?? 'Nothing to show'}
            </div>
          ) : (
            options.map((option) => {
              const isSelected = option.value === selected
              return (
                <button
                  key={option.value ?? '__all__'}
                  type="button"
                  onClick={() => {
                    onSelect(option.value)
                    setOpen(false)
                  }}
                  className="flex items-center gap-2 px-2 py-1.5 rounded text-meta text-foreground hover:bg-muted transition-colors text-left"
                >
                  <Check
                    className={cn(
                      'h-3.5 w-3.5 shrink-0',
                      isSelected ? 'opacity-100 text-primary' : 'opacity-0'
                    )}
                  />
                  <span className="font-mono truncate">{option.label}</span>
                  {option.hint && (
                    <span className="ml-auto shrink-0 text-caps text-muted-foreground">
                      {option.hint}
                    </span>
                  )}
                </button>
              )
            })
          )}
        </div>
      )}
    </div>
  )
}

const ALL_NAMESPACES = 'All namespaces'

interface ClusterBarProps {
  contexts: KubeContext[]
  currentContext: string | null
  selectedContext: string | null
  onSelectContext: (context: string | null) => void
  namespaces: string[]
  selectedNamespace: string | null
  onSelectNamespace: (namespace: string | null) => void
  namespacesLoading: boolean
}

export default function ClusterBar({
  contexts,
  currentContext,
  selectedContext,
  onSelectContext,
  namespaces,
  selectedNamespace,
  onSelectNamespace,
  namespacesLoading,
}: ClusterBarProps) {
  const effectiveContext = selectedContext ?? currentContext
  const contextOptions: DropdownOption[] = contexts.map((context) => ({
    value: context.name,
    label: context.name,
    hint: context.name === currentContext ? 'current' : undefined,
  }))
  const namespaceOptions: DropdownOption[] = [
    { value: null, label: ALL_NAMESPACES },
    ...namespaces.map((namespace) => ({ value: namespace, label: namespace })),
  ]

  return (
    <div className="shrink-0 flex items-center gap-2 px-4 sm:px-6 lg:px-8 h-10 border-b border-border bg-background/60">
      <Dropdown
        icon={<Server className="h-3 w-3" />}
        label="Context"
        value={effectiveContext ?? 'default'}
        options={contextOptions}
        selected={effectiveContext}
        onSelect={onSelectContext}
        emptyLabel="No contexts in kubeconfig"
      />
      <Dropdown
        icon={<Layers className="h-3 w-3" />}
        label="Namespace"
        value={namespacesLoading ? 'Loading…' : (selectedNamespace ?? ALL_NAMESPACES)}
        options={namespaceOptions}
        selected={selectedNamespace}
        onSelect={onSelectNamespace}
        disabled={namespacesLoading}
        emptyLabel="No namespaces visible"
      />
      <span className="text-caps text-muted-foreground/70 truncate hidden md:inline">
        Local sessions are always shown
      </span>
    </div>
  )
}
