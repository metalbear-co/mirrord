import { Code } from '@metalbear/ui'

export default function ConfigTab({ config }: { config: unknown }) {
  return (
    <div className="p-4 overflow-auto h-full">
      <Code
        variant="block"
        language="json"
        className="text-[11px] whitespace-pre-wrap bg-card/30 border border-border"
      >
        {JSON.stringify(config, null, 2)}
      </Code>
    </div>
  )
}
