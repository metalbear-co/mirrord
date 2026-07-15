import { useEffect, useRef, useState } from 'react'
import { cn } from '@metalbear/ui'
import { Check, ChevronDown } from 'lucide-react'
import type { KubeContext } from '../types'

const ALL_NAMESPACES = 'All namespaces'

const triggerClass =
  'inline-flex items-center gap-1.5 rounded-full border border-border bg-muted/50 hover:bg-muted px-2.5 h-7 max-w-[220px] transition-colors'

interface DropdownOption {
  value: string | null
  label: string
  hint?: string
}

interface DropdownProps {
  label: string
  value: string
  options: DropdownOption[]
  selected: string | null
  onSelect: (value: string | null) => void
  emptyLabel?: string
}

/** Plain select-from-list dropdown, used for the context (always listable from the kubeconfig). */
function Dropdown({
  label,
  value,
  options,
  selected,
  onSelect,
  emptyLabel,
}: DropdownProps) {
  const [open, setOpen] = useState(false)
  const ref = useRef<HTMLDivElement>(null)
  useCloseOnOutside(ref, open, () => setOpen(false))

  return (
    <div ref={ref} className="relative min-w-0">
      <button
        type="button"
        onClick={() => setOpen((o) => !o)}
        title={`${label}: ${value}`}
        className={cn(triggerClass, 'cursor-pointer')}
      >
        <span className="text-caps text-muted-foreground shrink-0">
          {label}
        </span>
        <span className="text-meta text-foreground font-medium truncate">
          {value}
        </span>
        <ChevronDown className="h-3 w-3 shrink-0 text-muted-foreground" />
      </button>

      {open && (
        <div className="absolute right-0 top-full mt-1.5 z-50 min-w-[220px] max-w-[360px] max-h-[320px] overflow-y-auto rounded-lg border border-border bg-popover text-popover-foreground shadow-lg p-1 flex flex-col">
          {options.length === 0 ? (
            <div className="px-2 py-2 text-meta text-muted-foreground">
              {emptyLabel ?? 'Nothing to show'}
            </div>
          ) : (
            options.map((option) => (
              <OptionRow
                key={option.value ?? '__all__'}
                label={option.label}
                hint={option.hint}
                selected={option.value === selected}
                onClick={() => {
                  onSelect(option.value)
                  setOpen(false)
                }}
              />
            ))
          )}
        </div>
      )}
    </div>
  )
}

interface NamespaceComboboxProps {
  selectedNamespace: string | null
  namespaces: string[]
  onSelect: (namespace: string | null) => void
  loading: boolean
  error: boolean
}

/**
 * Namespace picker that doubles as a free-text input. Listing namespaces can be denied by RBAC on
 * strict multi-tenant clusters; when it is (or when the wanted namespace just isn't listed), the
 * user can type any namespace and apply it.
 */
function NamespaceCombobox({
  selectedNamespace,
  namespaces,
  onSelect,
  loading,
  error,
}: NamespaceComboboxProps) {
  const [open, setOpen] = useState(false)
  const [text, setText] = useState('')
  const ref = useRef<HTMLDivElement>(null)
  useCloseOnOutside(ref, open, () => {
    setOpen(false)
    setText('')
  })

  const value =
    loading && !error ? 'Loading…' : (selectedNamespace ?? ALL_NAMESPACES)
  const query = text.trim()
  const filtered = query
    ? namespaces.filter((n) => n.toLowerCase().includes(query.toLowerCase()))
    : namespaces
  const canUseCustom = query.length > 0 && !namespaces.includes(query)

  const apply = (namespace: string | null) => {
    onSelect(namespace)
    setOpen(false)
    setText('')
  }

  return (
    <div ref={ref} className="relative min-w-0">
      <button
        type="button"
        onClick={() => setOpen((o) => !o)}
        title={`Namespace: ${value}`}
        className={cn(triggerClass, 'cursor-pointer')}
      >
        <span className="text-caps text-muted-foreground shrink-0">
          Namespace
        </span>
        <span className="text-meta text-foreground font-medium truncate">
          {value}
        </span>
        <ChevronDown className="h-3 w-3 shrink-0 text-muted-foreground" />
      </button>

      {open && (
        <div className="absolute right-0 top-full mt-1.5 z-50 min-w-[240px] max-w-[360px] rounded-lg border border-border bg-popover text-popover-foreground shadow-lg p-1 flex flex-col">
          <input
            autoFocus
            value={text}
            onChange={(e) => setText(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter' && query.length > 0) apply(query)
            }}
            placeholder="Filter or type a namespace…"
            className="mb-1 h-7 rounded border border-border bg-background px-2 text-meta text-foreground outline-none focus:border-primary"
          />
          {error && (
            <div className="px-2 py-1 text-meta text-destructive">
              Couldn't list namespaces — type one to use it.
            </div>
          )}
          <div className="max-h-[280px] overflow-y-auto flex flex-col">
            {canUseCustom && (
              <OptionRow
                label={`Use "${query}"`}
                selected={false}
                onClick={() => apply(query)}
              />
            )}
            {!query && (
              <OptionRow
                label={ALL_NAMESPACES}
                selected={selectedNamespace === null}
                onClick={() => apply(null)}
              />
            )}
            {filtered.map((namespace) => (
              <OptionRow
                key={namespace}
                label={namespace}
                selected={namespace === selectedNamespace}
                onClick={() => apply(namespace)}
              />
            ))}
            {filtered.length === 0 && !canUseCustom && !error && (
              <div className="px-2 py-2 text-meta text-muted-foreground">
                {loading ? 'Loading namespaces…' : 'No namespaces visible'}
              </div>
            )}
          </div>
        </div>
      )}
    </div>
  )
}

function OptionRow({
  label,
  hint,
  selected,
  onClick,
}: {
  label: string
  hint?: string
  selected: boolean
  onClick: () => void
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className="flex items-center gap-2 px-2 py-1.5 rounded text-meta text-foreground hover:bg-muted transition-colors text-left"
    >
      <Check
        className={cn(
          'h-3.5 w-3.5 shrink-0',
          selected ? 'opacity-100 text-primary' : 'opacity-0',
        )}
      />
      <span className="font-mono truncate">{label}</span>
      {hint && (
        <span className="ml-auto shrink-0 text-caps text-muted-foreground">
          {hint}
        </span>
      )}
    </button>
  )
}

/** Closes the menu on an outside click or Escape while `open`. */
function useCloseOnOutside(
  ref: React.RefObject<HTMLElement | null>,
  open: boolean,
  close: () => void,
) {
  useEffect(() => {
    if (!open) return
    const onClick = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) close()
    }
    const onEsc = (e: KeyboardEvent) => {
      if (e.key === 'Escape') close()
    }
    document.addEventListener('mousedown', onClick)
    document.addEventListener('keydown', onEsc)
    return () => {
      document.removeEventListener('mousedown', onClick)
      document.removeEventListener('keydown', onEsc)
    }
  }, [ref, open, close])
}

interface ContextNamespacePickerProps {
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

/**
 * Context and namespace pickers, rendered inline in the header next to the user menu. They scope the
 * cluster (operator) session view; local sessions are always shown regardless of the selection.
 */
export default function ContextNamespacePicker({
  contexts,
  currentContext,
  selectedContext,
  onSelectContext,
  namespaces,
  selectedNamespace,
  onSelectNamespace,
  namespacesLoading,
  namespacesError,
}: ContextNamespacePickerProps) {
  const effectiveContext = selectedContext ?? currentContext
  const contextOptions: DropdownOption[] = contexts.map((context) => ({
    value: context.name,
    label: context.name,
    hint: context.name === currentContext ? 'current' : undefined,
  }))

  return (
    <div className="hidden md:flex items-center gap-2 min-w-0">
      <Dropdown
        label="Context"
        value={effectiveContext ?? 'default'}
        options={contextOptions}
        selected={effectiveContext}
        onSelect={onSelectContext}
        emptyLabel="No contexts in kubeconfig"
      />
      <NamespaceCombobox
        selectedNamespace={selectedNamespace}
        namespaces={namespaces}
        onSelect={onSelectNamespace}
        loading={namespacesLoading}
        error={namespacesError}
      />
    </div>
  )
}
