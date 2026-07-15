import { useEffect, useRef, useState } from 'react'
import { cn } from '@metalbear/ui'
import { Check, ChevronDown } from 'lucide-react'
import type { KubeContext } from '../types'
import { strings } from '../strings'

const ALL_NAMESPACES = 'All namespaces'

const triggerClass =
  'inline-flex items-center gap-1.5 rounded-full border border-border bg-muted/50 hover:bg-muted px-2.5 h-7 max-w-[220px] transition-colors'

interface DropdownOption {
  value: string | null
  label: string
  hint?: string | undefined
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
        <span className="text-meta text-foreground truncate font-medium">
          {value}
        </span>
        <ChevronDown className="text-muted-foreground h-3 w-3 shrink-0" />
      </button>

      {open && (
        <div className="border-border bg-popover text-popover-foreground absolute right-0 top-full z-50 mt-1.5 flex max-h-[320px] min-w-[220px] max-w-[360px] flex-col overflow-y-auto rounded-lg border p-1 shadow-lg">
          {options.length === 0 ? (
            <div className="text-meta text-muted-foreground px-2 py-2">
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
  const inputRef = useRef<HTMLInputElement>(null)
  useCloseOnOutside(ref, open, () => {
    setOpen(false)
    setText('')
  })
  useEffect(() => {
    if (open) inputRef.current?.focus()
  }, [open])

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
          {strings.namespacePicker.namespace}
        </span>
        <span className="text-meta text-foreground truncate font-medium">
          {value}
        </span>
        <ChevronDown className="text-muted-foreground h-3 w-3 shrink-0" />
      </button>

      {open && (
        <div className="border-border bg-popover text-popover-foreground absolute right-0 top-full z-50 mt-1.5 flex min-w-[240px] max-w-[360px] flex-col rounded-lg border p-1 shadow-lg">
          <input
            ref={inputRef}
            value={text}
            onChange={(e) => setText(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter' && query.length > 0) apply(query)
            }}
            placeholder="Filter or type a namespace…"
            className="border-border bg-background text-meta text-foreground focus:border-primary mb-1 h-7 rounded border px-2 outline-none"
          />
          {error && (
            <div className="text-meta text-destructive px-2 py-1">
              {strings.namespacePicker.listError}
            </div>
          )}
          <div className="flex max-h-[280px] flex-col overflow-y-auto">
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
              <div className="text-meta text-muted-foreground px-2 py-2">
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
  hint?: string | undefined
  selected: boolean
  onClick: () => void
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className="text-meta text-foreground hover:bg-muted flex items-center gap-2 rounded px-2 py-1.5 text-left transition-colors"
    >
      <Check
        className={cn(
          'h-3.5 w-3.5 shrink-0',
          selected ? 'text-primary opacity-100' : 'opacity-0',
        )}
      />
      <span className="truncate font-mono">{label}</span>
      {hint && (
        <span className="text-caps text-muted-foreground ml-auto shrink-0">
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
    <div className="hidden min-w-0 items-center gap-2 md:flex">
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
