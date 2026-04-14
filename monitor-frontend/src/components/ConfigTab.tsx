export default function ConfigTab({ config }: { config: unknown }) {
  return (
    <div className="p-4 overflow-auto h-full">
      <pre className="p-4 text-[11px] font-mono text-foreground/80 whitespace-pre-wrap overflow-auto rounded-lg border border-border bg-card/30">
        {JSON.stringify(config, null, 2)}
      </pre>
    </div>
  )
}
