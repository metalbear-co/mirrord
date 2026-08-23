interface PortModeChipProps {
  port: number
  mode: string
  filter?: string | null | undefined
}

// Shared between the own-session and shared-operator views so a port's mode/filter render
// identically regardless of which data source it came from.
export default function PortModeChip({
  port,
  mode,
  filter,
}: PortModeChipProps) {
  const tooltip = filter ? `${mode} :${port} · ${filter}` : `${mode} :${port}`
  return (
    <span
      className="border-border bg-card/40 text-meta inline-flex items-center gap-1.5 rounded-full border px-2 py-0.5 font-mono"
      title={tooltip}
    >
      <span className="text-muted-foreground text-caps">{mode}</span>
      <span className="text-foreground font-medium">:{port}</span>
      {filter && (
        <span className="text-muted-foreground/70 max-w-[120px] truncate">
          {filter}
        </span>
      )}
    </span>
  )
}
